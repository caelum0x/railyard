use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use serde_json::json;
use switchyard_meter::mint::decode_receipt_header;
use switchyard_meter::{MintBalance, MintError, MintHold, MintReceipt, MintTransport};

#[derive(Debug, Clone, PartialEq)]
pub struct FakeHoldState {
    pub account: String,
    pub amount: i64,
    pub state: &'static str,
    pub captured: i64,
    pub to: Option<String>,
}

/// An in-memory Mint: accounts with available/held balances, idempotent holds.
#[derive(Default)]
pub struct FakeMint {
    accounts: Mutex<HashMap<String, i64>>,
    holds: Mutex<HashMap<String, FakeHoldState>>,
    idem: Mutex<HashMap<String, String>>,
    seq: AtomicU64,
    pub unavailable: AtomicBool,
    pub checkouts: Mutex<Vec<(String, i64, String)>>,
}

impl FakeMint {
    pub fn new() -> std::sync::Arc<Self> {
        std::sync::Arc::new(FakeMint::default())
    }

    pub fn fund(&self, account: &str, micros: i64) {
        *self.accounts.lock().unwrap().entry(account.into()).or_default() += micros;
    }

    pub fn available(&self, account: &str) -> i64 {
        *self.accounts.lock().unwrap().get(account).unwrap_or(&0)
    }

    pub fn holds(&self) -> Vec<(String, FakeHoldState)> {
        self.holds.lock().unwrap().iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    pub fn open_holds(&self) -> usize {
        self.holds.lock().unwrap().values().filter(|h| h.state == "open").count()
    }

    fn receipt(&self, op: &str, amount: i64) -> MintReceipt {
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        let r = json!({"seq": seq, "op": op, "op_hash": format!("op{seq}"), "amount_micros": amount, "hash": format!("h{seq}"), "sig": "fake"});
        let (receipt_id, receipt) = decode_receipt_header(Some(&URL_SAFE_NO_PAD.encode(r.to_string())));
        MintReceipt { amount_micros: amount, receipt_id, receipt }
    }

    fn up(&self) -> Result<(), MintError> {
        if self.unavailable.load(Ordering::SeqCst) {
            Err(MintError::Unavailable("fake mint down".into()))
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl MintTransport for FakeMint {
    async fn create_hold(&self, account_id: &str, amount_micros: i64, idem_key: &str) -> Result<MintHold, MintError> {
        self.up()?;
        if let Some(existing) = self.idem.lock().unwrap().get(idem_key) {
            return Ok(MintHold { hold_id: existing.clone(), amount_micros });
        }
        let mut accounts = self.accounts.lock().unwrap();
        let bal = accounts.get_mut(account_id).ok_or(MintError::AccountNotFound)?;
        if *bal < amount_micros {
            return Err(MintError::InsufficientFunds);
        }
        *bal -= amount_micros;
        let id = format!("hold_{}", self.seq.fetch_add(1, Ordering::SeqCst) + 1);
        self.holds.lock().unwrap().insert(id.clone(), FakeHoldState { account: account_id.into(), amount: amount_micros, state: "open", captured: 0, to: None });
        self.idem.lock().unwrap().insert(idem_key.into(), id.clone());
        Ok(MintHold { hold_id: id, amount_micros })
    }

    async fn capture(&self, hold_id: &str, amount_micros: i64, to_account: &str, _idem_key: &str) -> Result<MintReceipt, MintError> {
        self.up()?;
        let mut holds = self.holds.lock().unwrap();
        let h = holds.get_mut(hold_id).ok_or(MintError::HoldNotFound)?;
        if h.state != "open" {
            return Ok(self.receipt("capture", h.captured));
        }
        if amount_micros > h.amount {
            return Err(MintError::Other { status: 422, code: "capture_exceeds_hold".into(), message: "capture > hold".into() });
        }
        let refund = h.amount - amount_micros;
        h.state = "captured";
        h.captured = amount_micros;
        h.to = Some(to_account.into());
        let account = h.account.clone();
        drop(holds);
        let mut accounts = self.accounts.lock().unwrap();
        *accounts.entry(account).or_default() += refund;
        *accounts.entry(to_account.into()).or_default() += amount_micros;
        drop(accounts);
        Ok(self.receipt("capture", amount_micros))
    }

    async fn release(&self, hold_id: &str, _idem_key: &str) -> Result<MintReceipt, MintError> {
        self.up()?;
        let mut holds = self.holds.lock().unwrap();
        let h = holds.get_mut(hold_id).ok_or(MintError::HoldNotFound)?;
        if h.state != "open" {
            return Ok(self.receipt("release", 0));
        }
        h.state = "released";
        let (account, amount) = (h.account.clone(), h.amount);
        drop(holds);
        *self.accounts.lock().unwrap().entry(account).or_default() += amount;
        Ok(self.receipt("release", amount))
    }

    async fn balance(&self, account_id: &str) -> Result<MintBalance, MintError> {
        self.up()?;
        let held = self.holds.lock().unwrap().values().filter(|h| h.account == account_id && h.state == "open").map(|h| h.amount).sum();
        Ok(MintBalance { available_micros: self.available(account_id), held_micros: held })
    }

    async fn checkout(&self, account_id: &str, amount_micros: i64, provider: &str, _success_url: Option<&str>) -> Result<String, MintError> {
        self.up()?;
        self.checkouts.lock().unwrap().push((account_id.into(), amount_micros, provider.into()));
        Ok(format!("https://checkout.{provider}.test/pay?account={account_id}&amount={amount_micros}"))
    }
}
