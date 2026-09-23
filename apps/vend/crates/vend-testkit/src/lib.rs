//! # vend-testkit — in-memory transports + repo + fixtures (no network, no DB).
//!
//! [`FakeMint`] and [`FakeRelay`] implement the [`MintTransport`]/[`RelayTransport`]
//! seams with scriptable balances, forced `402`s, canned usage, and idempotent
//! replay. [`InMemoryStore`] implements the whole repo-port surface. [`fixtures`]
//! seeds two isolated tenants with keys and policies.

pub mod fixtures;
pub mod store;

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use vend_core::error::VendError;
use vend_core::transport::{
    CaptureRequest, CaptureResponse, CheckoutRequest, CheckoutResponse, HoldRequest, HoldResponse,
    HoldStatus, HoldView, MintBalance, MintTransport, RelayRequest, RelayResponse, RelayTransport,
    ReleaseRequest, ReleaseResponse, SettlementProof, TransferRequest, TransferResponse, Usage,
};

pub use store::InMemoryStore;

// --- FakeMint -------------------------------------------------------------

#[derive(Clone)]
struct HoldRec {
    account: String,
    amount: i64,
    settled: bool,
    captured_micros: i64,
}

struct MintState {
    balances: HashMap<String, i64>,
    holds: HashMap<String, HoldRec>,
    by_idem: HashMap<String, String>,
    transfers: HashMap<String, (TransferRequest, TransferResponse)>,
    force_insufficient: bool,
    seq: u64,
}

/// A scriptable in-memory Mint. Balances are set up-front; holds move balance
/// into reserve; capture consumes it; release returns it. Idempotent by
/// `idem_key`.
pub struct FakeMint {
    inner: Mutex<MintState>,
}

impl Default for FakeMint {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeMint {
    /// A fresh Mint with no accounts.
    pub fn new() -> Self {
        FakeMint {
            inner: Mutex::new(MintState {
                balances: HashMap::new(),
                holds: HashMap::new(),
                by_idem: HashMap::new(),
                transfers: HashMap::new(),
                force_insufficient: false,
                seq: 0,
            }),
        }
    }

    /// Seed an account with an available balance (µ$).
    pub fn with_balance(self, account: &str, micros: i64) -> Self {
        self.inner
            .lock()
            .unwrap()
            .balances
            .insert(account.to_string(), micros);
        self
    }

    /// Force every `create_hold` to fail with `InsufficientFunds`.
    pub fn forcing_insufficient(self) -> Self {
        self.inner.lock().unwrap().force_insufficient = true;
        self
    }

    /// Current available balance for an account.
    pub fn balance(&self, account: &str) -> i64 {
        *self
            .inner
            .lock()
            .unwrap()
            .balances
            .get(account)
            .unwrap_or(&0)
    }
}

#[async_trait]
impl MintTransport for FakeMint {
    async fn create_checkout(&self, req: CheckoutRequest) -> Result<CheckoutResponse, VendError> {
        if req.amount_micros <= 0 || req.amount_micros % 10_000 != 0 {
            return Err(VendError::InvalidAmount);
        }
        Ok(CheckoutResponse {
            provider: req.provider.clone(),
            checkout_id: "checkout_test".into(),
            checkout_url: "https://pay.test/checkout_test".into(),
            deposit_ref: format!("{}:checkout_test", req.provider),
            account_id: req.account_id,
            amount_micros: req.amount_micros,
        })
    }

    async fn create_hold(&self, req: HoldRequest) -> Result<HoldResponse, VendError> {
        let mut s = self.inner.lock().unwrap();
        if let Some(existing) = s.by_idem.get(&req.idem_key).cloned() {
            return Ok(HoldResponse { hold_id: existing });
        }
        if s.force_insufficient {
            return Err(VendError::InsufficientFunds);
        }
        let bal = *s.balances.get(&req.from).unwrap_or(&0);
        if bal < req.amount_micros {
            return Err(VendError::InsufficientFunds);
        }
        s.balances.insert(req.from.clone(), bal - req.amount_micros);
        s.seq += 1;
        let hold_id = format!("hold_{:026}", s.seq);
        s.holds.insert(
            hold_id.clone(),
            HoldRec {
                account: req.from.clone(),
                amount: req.amount_micros,
                settled: false,
                captured_micros: 0,
            },
        );
        s.by_idem.insert(req.idem_key.clone(), hold_id.clone());
        Ok(HoldResponse { hold_id })
    }

    async fn get_hold(&self, hold_id: &str) -> Result<HoldView, VendError> {
        let s = self.inner.lock().unwrap();
        let rec = s.holds.get(hold_id).ok_or(VendError::NotFound)?;
        Ok(HoldView {
            id: hold_id.to_string(),
            account_id: rec.account.clone(),
            amount_micros: rec.amount,
            captured_micros: rec.captured_micros,
            status: if !rec.settled {
                HoldStatus::Open
            } else if rec.captured_micros > 0 {
                HoldStatus::Captured
            } else {
                HoldStatus::Released
            },
            ref_: None,
        })
    }

    async fn capture_hold(
        &self,
        hold_id: &str,
        req: CaptureRequest,
    ) -> Result<CaptureResponse, VendError> {
        let mut s = self.inner.lock().unwrap();
        let rec = s.holds.get(hold_id).cloned().ok_or(VendError::NotFound)?;
        if req.amount_micros > rec.amount {
            return Err(VendError::CaptureExceedsHold);
        }
        if !rec.settled {
            // Return the unused remainder of the hold to available balance.
            let refund = rec.amount - req.amount_micros;
            let bal = *s.balances.get(&rec.account).unwrap_or(&0);
            s.balances.insert(rec.account.clone(), bal + refund);
            if let Some(h) = s.holds.get_mut(hold_id) {
                h.settled = true;
                h.captured_micros = req.amount_micros;
            }
            let payee = *s.balances.get(&req.to).unwrap_or(&0);
            s.balances.insert(req.to.clone(), payee + req.amount_micros);
        }
        s.seq += 1;
        let sequence = s.seq;
        Ok(CaptureResponse {
            captured_micros: req.amount_micros,
            proof: SettlementProof {
                sequence,
                operation: "hold.capture".into(),
                operation_hash: format!("fake-op-{sequence}"),
                receipt_hash: format!("fake-receipt-{sequence}"),
                signature: format!("fake-signature-{sequence}"),
            },
        })
    }

    async fn release_hold(
        &self,
        hold_id: &str,
        _req: ReleaseRequest,
    ) -> Result<ReleaseResponse, VendError> {
        let mut s = self.inner.lock().unwrap();
        let rec = s.holds.get(hold_id).cloned().ok_or(VendError::NotFound)?;
        if !rec.settled {
            let bal = *s.balances.get(&rec.account).unwrap_or(&0);
            s.balances.insert(rec.account.clone(), bal + rec.amount);
            if let Some(h) = s.holds.get_mut(hold_id) {
                h.settled = true;
            }
        }
        s.seq += 1;
        let sequence = s.seq;
        Ok(ReleaseResponse {
            proof: SettlementProof {
                sequence,
                operation: "hold.release".into(),
                operation_hash: format!("fake-op-{sequence}"),
                receipt_hash: format!("fake-receipt-{sequence}"),
                signature: format!("fake-signature-{sequence}"),
            },
        })
    }

    async fn transfer(&self, req: TransferRequest) -> Result<TransferResponse, VendError> {
        let mut s = self.inner.lock().unwrap();
        if let Some((existing, response)) = s.transfers.get(&req.idem_key) {
            return if existing == &req {
                let mut replay = response.clone();
                replay.idempotent_replay = true;
                Ok(replay)
            } else {
                Err(VendError::IdempotencyConflict)
            };
        }
        if req.amount_micros <= 0 {
            return Err(VendError::InvalidAmount);
        }
        let from = *s.balances.get(&req.from).unwrap_or(&0);
        if from < req.amount_micros {
            return Err(VendError::InsufficientFunds);
        }
        let to = *s.balances.get(&req.to).unwrap_or(&0);
        s.balances
            .insert(req.from.clone(), from - req.amount_micros);
        s.balances.insert(req.to.clone(), to + req.amount_micros);
        s.seq += 2;
        let response = TransferResponse {
            first_seq: s.seq - 1,
            from_balance: MintBalance {
                account_id: req.from.clone(),
                available_micros: from - req.amount_micros,
                held_micros: 0,
                updated_seq: s.seq - 1,
            },
            to_balance: MintBalance {
                account_id: req.to.clone(),
                available_micros: to + req.amount_micros,
                held_micros: 0,
                updated_seq: s.seq,
            },
            idempotent_replay: false,
        };
        s.transfers
            .insert(req.idem_key.clone(), (req, response.clone()));
        Ok(response)
    }
    async fn get_balance(&self, account_id: &str) -> Result<MintBalance, VendError> {
        let s = self.inner.lock().unwrap();
        let held: i64 = s
            .holds
            .values()
            .filter(|h| h.account == account_id && !h.settled)
            .map(|h| h.amount)
            .sum();
        Ok(MintBalance {
            account_id: account_id.to_string(),
            available_micros: *s.balances.get(account_id).unwrap_or(&0),
            held_micros: held,
            updated_seq: s.seq,
        })
    }
}

// --- FakeRelay ------------------------------------------------------------

/// A scriptable in-memory Relay. Pushed responses are returned FIFO; when the
/// script is exhausted it returns [`FakeRelay::default_response`].
pub struct FakeRelay {
    scripted: Mutex<Vec<RelayResponse>>,
    default_response: RelayResponse,
}

impl Default for FakeRelay {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeRelay {
    /// A Relay that always returns a canned 200 with 1/1 token usage.
    pub fn new() -> Self {
        FakeRelay {
            scripted: Mutex::new(Vec::new()),
            default_response: RelayResponse {
                status: 200,
                body: serde_json::json!({"choices": []}),
                usage: Some(Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                    provider_cost_micros: Some(20),
                }),
            },
        }
    }

    /// Script the next response (FIFO).
    pub fn push(self, resp: RelayResponse) -> Self {
        self.scripted.lock().unwrap().insert(0, resp);
        self
    }

    /// A canned 2xx response carrying the given usage.
    pub fn canned_success(usage: Usage) -> RelayResponse {
        RelayResponse {
            status: 200,
            body: serde_json::json!({"choices": []}),
            usage: Some(usage),
        }
    }

    /// A canned non-2xx (no usage) response.
    pub fn canned_error(status: u16) -> RelayResponse {
        RelayResponse {
            status,
            body: serde_json::json!({"error": "upstream"}),
            usage: None,
        }
    }
}

#[async_trait]
impl RelayTransport for FakeRelay {
    async fn chat_completion(&self, _req: RelayRequest) -> Result<RelayResponse, VendError> {
        let mut s = self.scripted.lock().unwrap();
        Ok(s.pop().unwrap_or_else(|| self.default_response.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hold_capture_consumes_exact_actual() {
        let mint = FakeMint::new().with_balance("acct_a", 10_000);
        let hold = mint
            .create_hold(HoldRequest {
                from: "acct_a".into(),
                amount_micros: 6_000,
                idem_key: "job1".into(),
            })
            .await
            .unwrap();
        assert_eq!(mint.balance("acct_a"), 4_000);
        mint.capture_hold(
            &hold.hold_id,
            CaptureRequest {
                amount_micros: 2_000,
                to: "acct_settlement".into(),
                idem_key: "job1:capture".into(),
            },
        )
        .await
        .unwrap();
        // Unused 4_000 of the 6_000 hold returns; 2_000 stays captured.
        assert_eq!(mint.balance("acct_a"), 8_000);
    }

    #[tokio::test]
    async fn idempotent_hold_replay_returns_same_id() {
        let mint = FakeMint::new().with_balance("acct_a", 10_000);
        let req = HoldRequest {
            from: "acct_a".into(),
            amount_micros: 1_000,
            idem_key: "same".into(),
        };
        let a = mint.create_hold(req.clone()).await.unwrap();
        let b = mint.create_hold(req).await.unwrap();
        assert_eq!(a.hold_id, b.hold_id);
        assert_eq!(mint.balance("acct_a"), 9_000);
    }

    #[tokio::test]
    async fn forced_insufficient_short_circuits() {
        let mint = FakeMint::new()
            .with_balance("acct_a", 10_000)
            .forcing_insufficient();
        let err = mint
            .create_hold(HoldRequest {
                from: "acct_a".into(),
                amount_micros: 1,
                idem_key: "j".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(err, VendError::InsufficientFunds);
    }
}
