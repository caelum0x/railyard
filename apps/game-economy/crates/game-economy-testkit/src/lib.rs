//! [`FakeMint`] — an in-memory Mint that enforces the REAL ledger's hold
//! semantics, so tests prove the game store against the same contract prod runs.
//!
//! Faithful to `mint-store`'s memory store (`apps/mint`):
//! - `create_hold` moves `available -> held` atomically; over-balance is
//!   `InsufficientFunds`.
//! - `capture_hold` settles up to the held amount to the payee, refunds the
//!   remainder, and closes the hold **`Captured` (terminal)**.
//! - `release_hold` frees the whole reservation with no settlement and closes the
//!   hold **`Released` (terminal)**.
//! - Any close of an already-terminal hold is **`HoldNotFound`** (Mint collapses
//!   "distinct close of a terminal hold" to not-found).
//! - One global idempotency index across all ops: a replay under the same
//!   `idem_key` returns the ORIGINAL outcome (`idempotent_replay = true`, no state
//!   change); the same key with a different body is an `IdempotencyConflict`.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use game_economy_core::error::GameError;
use game_economy_core::transport::{
    AccountBalance, Capture, CaptureReceipt, Hold, HoldReceipt, MintTransport, Refund, RefundReceipt, Release,
    ReleaseReceipt, SettlementProof,
};

/// A mutating transport call, recorded in order for test assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    /// `create_hold(account, amount)`.
    CreateHold { account: String, amount: i64 },
    /// `capture_hold(hold_id, amount, to)`.
    Capture {
        hold_id: String,
        amount: i64,
        to: String,
    },
    /// `release_hold(hold_id)`.
    Release { hold_id: String },
    /// `refund(from -> to, amount)` — a direct transfer back to the player.
    Refund { from: String, to: String, amount: i64 },
}

/// The lifecycle of a hold, mirroring `mint_store::HoldStatus`.
#[derive(Copy, Clone, PartialEq, Eq, Debug)]
enum HoldStatus {
    Open,
    Captured,
    Released,
}

#[derive(Clone)]
struct HoldRec {
    account: String,
    amount: i64,
    status: HoldStatus,
}

/// A stored idempotency outcome, replayed verbatim on a same-key retry.
#[derive(Clone)]
enum StoredOutcome {
    Hold(HoldReceipt),
    Capture(CaptureReceipt),
    Release(ReleaseReceipt),
    Refund(RefundReceipt),
}

#[derive(Clone)]
struct StoredCommit {
    op_hash: String,
    outcome: StoredOutcome,
}

struct Ledger {
    /// Available (unheld) balance per account, in µ$.
    balances: HashMap<String, i64>,
    /// Holds by id.
    holds: HashMap<String, HoldRec>,
    /// Global idempotency index: idem_key -> stored outcome + body hash.
    idem: HashMap<String, StoredCommit>,
    /// Recorded calls, in order.
    calls: Vec<Call>,
    /// Monotonic ledger sequence.
    seq: u64,
    /// Monotonic hold counter (for minted ids).
    hold_seq: u64,
}

/// An in-memory, contract-faithful Mint for tests.
pub struct FakeMint {
    inner: Mutex<Ledger>,
}

impl Default for FakeMint {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeMint {
    /// A fresh ledger with no accounts.
    pub fn new() -> Self {
        FakeMint {
            inner: Mutex::new(Ledger {
                balances: HashMap::new(),
                holds: HashMap::new(),
                idem: HashMap::new(),
                calls: Vec::new(),
                seq: 0,
                hold_seq: 0,
            }),
        }
    }

    /// Seed an account with an available balance (µ$). Chainable.
    pub fn with_balance(self, account: &str, micros: i64) -> Self {
        self.inner
            .lock()
            .unwrap()
            .balances
            .insert(account.to_string(), micros);
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

    /// Sum of every account's available (unheld) balance, in µ$. Read-only test
    /// introspection: the money-conservation invariant is
    /// `total_available() + held_total()` and must equal the seeded supply.
    pub fn total_available(&self) -> i64 {
        self.inner.lock().unwrap().balances.values().sum()
    }

    /// Sum of the amounts still reserved in **open** (non-terminal) holds, in µ$.
    /// A captured/released hold contributes nothing (its funds have settled or
    /// returned to available), so this counts only funds currently in-flight.
    pub fn held_total(&self) -> i64 {
        self.inner
            .lock()
            .unwrap()
            .holds
            .values()
            .filter(|h| h.status == HoldStatus::Open)
            .map(|h| h.amount)
            .sum()
    }

    /// Total conserved supply = available across all accounts + funds held open.
    /// Every hold/capture/release only *moves* money between these two buckets (or
    /// between accounts), so this value is invariant for the ledger's lifetime.
    pub fn conserved_total(&self) -> i64 {
        self.total_available() + self.held_total()
    }

    /// A snapshot of every account and its available balance, ordered by account
    /// id for deterministic assertions.
    pub fn accounts(&self) -> Vec<(String, i64)> {
        let mut out: Vec<(String, i64)> = self
            .inner
            .lock()
            .unwrap()
            .balances
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// The amounts of every currently-open hold, in µ$ (terminal holds excluded).
    pub fn open_holds(&self) -> Vec<i64> {
        self.inner
            .lock()
            .unwrap()
            .holds
            .values()
            .filter(|h| h.status == HoldStatus::Open)
            .map(|h| h.amount)
            .collect()
    }

    /// A snapshot of every recorded mutating call, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.inner.lock().unwrap().calls.clone()
    }

    /// Count of recorded calls matching a predicate — handy for "captured once".
    pub fn count_calls(&self, pred: impl Fn(&Call) -> bool) -> usize {
        self.inner
            .lock()
            .unwrap()
            .calls
            .iter()
            .filter(|c| pred(c))
            .count()
    }

    fn next_hold_id(l: &mut Ledger) -> String {
        l.hold_seq += 1;
        format!("hold_{:026}", l.hold_seq)
    }
}

/// Hash of an op's salient body, used for same-key/different-body conflict
/// detection — mirrors the ledger's `op_hash`.
fn op_hash(parts: &[&str]) -> String {
    let joined = parts.join("|");
    blake3::hash(joined.as_bytes()).to_hex()[..16].to_string()
}

#[async_trait]
impl MintTransport for FakeMint {
    async fn create_hold(&self, req: Hold) -> Result<HoldReceipt, GameError> {
        let mut l = self.inner.lock().unwrap();
        l.calls.push(Call::CreateHold {
            account: req.account_id.clone(),
            amount: req.amount_micros,
        });

        let oh = op_hash(&["create", &req.account_id, &req.amount_micros.to_string()]);
        if let Some(stored) = l.idem.get(&req.idem_key) {
            if stored.op_hash != oh {
                return Err(GameError::IdempotencyConflict);
            }
            if let StoredOutcome::Hold(r) = &stored.outcome {
                let mut r = r.clone();
                r.idempotent_replay = true;
                return Ok(r);
            }
            return Err(GameError::IdempotencyConflict);
        }

        // Atomic reserve: available -> held. Over-balance is a hard 402.
        let bal = *l.balances.get(&req.account_id).unwrap_or(&0);
        if bal < req.amount_micros {
            return Err(GameError::InsufficientFunds);
        }
        l.balances
            .insert(req.account_id.clone(), bal - req.amount_micros);

        let hold_id = Self::next_hold_id(&mut l);
        l.holds.insert(
            hold_id.clone(),
            HoldRec {
                account: req.account_id.clone(),
                amount: req.amount_micros,
                status: HoldStatus::Open,
            },
        );
        let receipt = HoldReceipt {
            hold_id,
            amount_micros: req.amount_micros,
            idempotent_replay: false,
        };
        l.idem.insert(
            req.idem_key.clone(),
            StoredCommit {
                op_hash: oh,
                outcome: StoredOutcome::Hold(receipt.clone()),
            },
        );
        Ok(receipt)
    }

    async fn capture_hold(&self, hold_id: &str, req: Capture) -> Result<CaptureReceipt, GameError> {
        let mut l = self.inner.lock().unwrap();
        l.calls.push(Call::Capture {
            hold_id: hold_id.to_string(),
            amount: req.amount_micros,
            to: req.to.clone(),
        });

        let oh = op_hash(&["capture", hold_id, &req.amount_micros.to_string(), &req.to]);
        if let Some(stored) = l.idem.get(&req.idem_key) {
            if stored.op_hash != oh {
                return Err(GameError::IdempotencyConflict);
            }
            if let StoredOutcome::Capture(r) = &stored.outcome {
                let mut r = r.clone();
                r.idempotent_replay = true;
                return Ok(r);
            }
            return Err(GameError::IdempotencyConflict);
        }

        // Resolve the hold; a distinct close of a terminal hold is not-found.
        let hold = l
            .holds
            .get(hold_id)
            .cloned()
            .ok_or(GameError::HoldNotFound)?;
        if hold.status != HoldStatus::Open {
            return Err(GameError::HoldNotFound);
        }
        if req.amount_micros > hold.amount {
            return Err(GameError::CaptureExceedsHold);
        }

        // Settle: refund the unused remainder to the holder, credit the payee,
        // close the hold Captured (terminal).
        let refund = hold.amount - req.amount_micros;
        let holder_bal = *l.balances.get(&hold.account).unwrap_or(&0);
        l.balances.insert(hold.account.clone(), holder_bal + refund);
        let payee_bal = *l.balances.get(&req.to).unwrap_or(&0);
        l.balances
            .insert(req.to.clone(), payee_bal + req.amount_micros);
        if let Some(h) = l.holds.get_mut(hold_id) {
            h.status = HoldStatus::Captured;
        }

        l.seq += 1;
        let seq = l.seq;
        let balance_after = *l.balances.get(&hold.account).unwrap_or(&0);
        let receipt_id = format!(
            "rcpt_{}",
            &blake3::hash(format!("receipt:v1:{hold_id}:{seq}").as_bytes()).to_hex()[..24]
        );
        let receipt = CaptureReceipt {
            captured_micros: req.amount_micros,
            balance_after_micros: balance_after,
            settlement_seq: seq,
            receipt_id: receipt_id.clone(),
            proof: SettlementProof {
                sequence: seq,
                operation: "hold.capture".into(),
                operation_hash: oh.clone(),
                receipt_hash: receipt_id.clone(),
                signature: format!("sig_fake_{seq}"),
            },
            idempotent_replay: false,
        };
        l.idem.insert(
            req.idem_key.clone(),
            StoredCommit {
                op_hash: oh,
                outcome: StoredOutcome::Capture(receipt.clone()),
            },
        );
        Ok(receipt)
    }

    async fn release_hold(&self, hold_id: &str, req: Release) -> Result<ReleaseReceipt, GameError> {
        let mut l = self.inner.lock().unwrap();
        l.calls.push(Call::Release {
            hold_id: hold_id.to_string(),
        });

        let oh = op_hash(&["release", hold_id]);
        if let Some(stored) = l.idem.get(&req.idem_key) {
            if stored.op_hash != oh {
                return Err(GameError::IdempotencyConflict);
            }
            if let StoredOutcome::Release(r) = &stored.outcome {
                let mut r = r.clone();
                r.idempotent_replay = true;
                return Ok(r);
            }
            return Err(GameError::IdempotencyConflict);
        }

        // Distinct close of a terminal (or missing) hold is not-found.
        let hold = l
            .holds
            .get(hold_id)
            .cloned()
            .ok_or(GameError::HoldNotFound)?;
        if hold.status != HoldStatus::Open {
            return Err(GameError::HoldNotFound);
        }

        // Free the whole reservation: held -> available, no payee.
        let holder_bal = *l.balances.get(&hold.account).unwrap_or(&0);
        l.balances
            .insert(hold.account.clone(), holder_bal + hold.amount);
        if let Some(h) = l.holds.get_mut(hold_id) {
            h.status = HoldStatus::Released;
        }
        l.seq += 1;
        let seq = l.seq;
        let receipt = ReleaseReceipt {
            released_micros: hold.amount,
            idempotent_replay: false,
            proof: SettlementProof {
                sequence: seq,
                operation: "hold.release".into(),
                operation_hash: oh.clone(),
                receipt_hash: format!("release_{}", &oh[..12]),
                signature: format!("sig_release_{}", &oh[..12]),
            },
        };
        l.idem.insert(
            req.idem_key.clone(),
            StoredCommit {
                op_hash: oh,
                outcome: StoredOutcome::Release(receipt.clone()),
            },
        );
        Ok(receipt)
    }

    async fn refund(&self, req: Refund) -> Result<RefundReceipt, GameError> {
        let mut l = self.inner.lock().unwrap();
        l.calls.push(Call::Refund {
            from: req.from.clone(),
            to: req.to.clone(),
            amount: req.amount_micros,
        });

        let oh = op_hash(&["transfer", &req.from, &req.to, &req.amount_micros.to_string()]);
        if let Some(stored) = l.idem.get(&req.idem_key) {
            if stored.op_hash != oh {
                return Err(GameError::IdempotencyConflict);
            }
            if let StoredOutcome::Refund(r) = &stored.outcome {
                let mut r = r.clone();
                r.idempotent_replay = true;
                return Ok(r);
            }
            return Err(GameError::IdempotencyConflict);
        }

        let from_bal = *l.balances.get(&req.from).unwrap_or(&0);
        if from_bal < req.amount_micros {
            return Err(GameError::InsufficientFunds);
        }
        l.balances.insert(req.from.clone(), from_bal - req.amount_micros);
        let to_bal = *l.balances.get(&req.to).unwrap_or(&0);
        l.balances.insert(req.to.clone(), to_bal + req.amount_micros);
        l.seq += 1;
        let seq = l.seq;
        let receipt = RefundReceipt {
            refunded_micros: req.amount_micros,
            settlement_seq: seq,
            receipt_hash: Some(format!("transfer_{}", &oh[..12])),
            idempotent_replay: false,
        };
        l.idem.insert(
            req.idem_key.clone(),
            StoredCommit {
                op_hash: oh,
                outcome: StoredOutcome::Refund(receipt.clone()),
            },
        );
        Ok(receipt)
    }

    async fn read_balance(&self, account_id: &str) -> Result<AccountBalance, GameError> {
        let l = self.inner.lock().unwrap();
        let available_micros = *l.balances.get(account_id).unwrap_or(&0);
        let held_micros = l
            .holds
            .values()
            .filter(|h| h.status == HoldStatus::Open && h.account == account_id)
            .map(|h| h.amount)
            .sum();
        Ok(AccountBalance {
            account_id: account_id.to_string(),
            available_micros,
            held_micros,
        })
    }

    async fn create_player_account(&self, _owner_ref: &str) -> Result<String, GameError> {
        let mut l = self.inner.lock().unwrap();
        l.seq += 1;
        let id = format!("acct_0{:026}", l.seq);
        l.balances.insert(id.clone(), 0);
        Ok(id)
    }
}
