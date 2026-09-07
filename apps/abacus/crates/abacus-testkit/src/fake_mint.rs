//! [`LedgerFakeMint`] — an in-memory Mint that ENFORCES the balance/allowance
//! ceiling invariant.
//!
//! The key property under test: a race-free enforcement gate. Every mutating call
//! takes a single serialized `Mutex` over the whole ledger, and `create_hold` /
//! `spend_allowance` drop spendable headroom ATOMICALLY at reserve time — so N
//! concurrent holds can never collectively overshoot the ceiling. When `est_max`
//! would breach the ceiling it returns a real `402` (`InsufficientFunds` for the
//! direct path, `AllowanceExceeded` for the ceiling path). Every call is recorded
//! for test assertions. Mirrors `vend-testkit::FakeMint` + `aitp stub.rs`.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use abacus_core::error::AbacusError;
use abacus_core::transport::{
    Allowance, AllowanceSpend, AllowanceSpendReceipt, Capture, CaptureReceipt, Hold, HoldReceipt,
    MintTransport, Release, ReleaseReceipt, SettlementProof,
};

/// Every mutating transport call, captured in order for assertions.
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
    /// `spend_allowance(allowance_id, amount)`.
    SpendAllowance { allowance_id: String, amount: i64 },
    /// `get_allowance(allowance_id)`.
    GetAllowance { allowance_id: String },
}

#[derive(Clone)]
struct HoldRec {
    account: String,
    amount: i64,
    /// Set once the hold reaches a terminal (captured/released) state.
    settled: bool,
    /// The allowance this hold draws against, if it was allowance-backed.
    allowance_id: Option<String>,
}

#[derive(Clone)]
struct AllowanceRec {
    limit: i64,
    spent: i64,
    revoked: bool,
}

struct Ledger {
    /// Available (unheld) balance per account, in µ$.
    balances: HashMap<String, i64>,
    /// Delegated allowance ceilings by id.
    allowances: HashMap<String, AllowanceRec>,
    /// Open + terminal holds by id.
    holds: HashMap<String, HoldRec>,
    /// Idempotency map: `idem_key` → `hold_id`.
    by_idem: HashMap<String, String>,
    /// Recorded calls, in order.
    calls: Vec<Call>,
    seq: u64,
}

/// An in-memory, invariant-enforcing Mint for tests.
pub struct LedgerFakeMint {
    inner: Mutex<Ledger>,
}

impl Default for LedgerFakeMint {
    fn default() -> Self {
        Self::new()
    }
}

impl LedgerFakeMint {
    /// A fresh ledger with no accounts or allowances.
    pub fn new() -> Self {
        LedgerFakeMint {
            inner: Mutex::new(Ledger {
                balances: HashMap::new(),
                allowances: HashMap::new(),
                holds: HashMap::new(),
                by_idem: HashMap::new(),
                calls: Vec::new(),
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

    /// Seed a delegated allowance with a ceiling and an already-spent amount (µ$).
    pub fn with_allowance(self, allowance_id: &str, limit: i64, spent: i64) -> Self {
        self.inner.lock().unwrap().allowances.insert(
            allowance_id.to_string(),
            AllowanceRec {
                limit,
                spent,
                revoked: false,
            },
        );
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

    /// Remaining allowance headroom (`limit - spent`, floored at 0).
    pub fn allowance_remaining(&self, allowance_id: &str) -> i64 {
        self.inner
            .lock()
            .unwrap()
            .allowances
            .get(allowance_id)
            .map(|a| (a.limit - a.spent).max(0))
            .unwrap_or(0)
    }

    /// A snapshot of every recorded call, in order.
    pub fn calls(&self) -> Vec<Call> {
        self.inner.lock().unwrap().calls.clone()
    }

    fn next_hold_id(l: &mut Ledger) -> String {
        l.seq += 1;
        format!("hold_{:026}", l.seq)
    }
}

#[async_trait]
impl MintTransport for LedgerFakeMint {
    async fn create_hold(&self, req: Hold) -> Result<HoldReceipt, AbacusError> {
        let mut l = self.inner.lock().unwrap();
        l.calls.push(Call::CreateHold {
            account: req.account_id.clone(),
            amount: req.amount_micros,
        });

        // Idempotent replay: the same idem_key returns the same hold.
        if let Some(existing) = l.by_idem.get(&req.idem_key).cloned() {
            let rec = l.holds.get(&existing).unwrap();
            return Ok(HoldReceipt {
                hold_id: existing,
                amount_micros: rec.amount,
            });
        }

        // Atomic reserve: spendable headroom drops the instant the hold opens, so
        // concurrent holds serialize through this lock and cannot overshoot.
        let bal = *l.balances.get(&req.account_id).unwrap_or(&0);
        if bal < req.amount_micros {
            return Err(AbacusError::InsufficientFunds);
        }
        l.balances
            .insert(req.account_id.clone(), bal - req.amount_micros);

        let hold_id = Self::next_hold_id(&mut l);
        l.holds.insert(
            hold_id.clone(),
            HoldRec {
                account: req.account_id.clone(),
                amount: req.amount_micros,
                settled: false,
                allowance_id: None,
            },
        );
        l.by_idem.insert(req.idem_key.clone(), hold_id.clone());
        Ok(HoldReceipt {
            hold_id,
            amount_micros: req.amount_micros,
        })
    }

    async fn capture_hold(
        &self,
        hold_id: &str,
        req: Capture,
    ) -> Result<CaptureReceipt, AbacusError> {
        let mut l = self.inner.lock().unwrap();
        l.calls.push(Call::Capture {
            hold_id: hold_id.to_string(),
            amount: req.amount_micros,
            to: req.payee.clone(),
        });
        let rec = l
            .holds
            .get(hold_id)
            .cloned()
            .ok_or(AbacusError::HoldNotFound)?;
        if req.amount_micros > rec.amount {
            return Err(AbacusError::CaptureExceedsHold);
        }
        if !rec.settled {
            // Return the unused remainder of the reservation to available balance.
            let refund = rec.amount - req.amount_micros;
            let bal = *l.balances.get(&rec.account).unwrap_or(&0);
            l.balances.insert(rec.account.clone(), bal + refund);
            // Finalize the allowance draw-down. `spend_allowance` already reserved
            // the FULL `rec.amount` against `spent`; capture converts that reserve
            // to the actual by returning the unused remainder — so only the captured
            // amount stays drawn down (spent -= (rec.amount - captured)). Adding the
            // captured amount here instead would double-count the reservation.
            if let Some(aid) = &rec.allowance_id {
                if let Some(a) = l.allowances.get_mut(aid) {
                    a.spent -= refund;
                }
            }
            if let Some(h) = l.holds.get_mut(hold_id) {
                h.settled = true;
            }
        }
        l.seq += 1;
        let seq = l.seq;
        let balance_after = *l.balances.get(&rec.account).unwrap_or(&0);
        Ok(CaptureReceipt {
            captured_micros: req.amount_micros,
            balance_after_micros: balance_after,
            settlement_seq: seq,
            proof: SettlementProof {
                sequence: seq,
                operation: "hold.capture".into(),
                operation_hash: format!("fake-op-{seq}"),
                receipt_hash: format!("fake-receipt-{seq}"),
                signature: format!("fake-signature-{seq}"),
            },
        })
    }

    async fn release_hold(
        &self,
        hold_id: &str,
        _req: Release,
    ) -> Result<ReleaseReceipt, AbacusError> {
        let mut l = self.inner.lock().unwrap();
        l.calls.push(Call::Release {
            hold_id: hold_id.to_string(),
        });
        let rec = l
            .holds
            .get(hold_id)
            .cloned()
            .ok_or(AbacusError::HoldNotFound)?;
        if !rec.settled {
            // Reservation returns in full; allowance headroom is restored because
            // an allowance-backed hold only draws `spent` on CAPTURE.
            let bal = *l.balances.get(&rec.account).unwrap_or(&0);
            l.balances.insert(rec.account.clone(), bal + rec.amount);
            if let Some(aid) = &rec.allowance_id {
                if let Some(a) = l.allowances.get_mut(aid) {
                    a.spent -= rec.amount;
                }
            }
            if let Some(h) = l.holds.get_mut(hold_id) {
                h.settled = true;
            }
        }
        l.seq += 1;
        let sequence = l.seq;
        Ok(ReleaseReceipt {
            released_micros: rec.amount,
            proof: SettlementProof {
                sequence,
                operation: "hold.release".into(),
                operation_hash: format!("fake-op-{sequence}"),
                receipt_hash: format!("fake-receipt-{sequence}"),
                signature: format!("fake-signature-{sequence}"),
            },
        })
    }

    async fn get_allowance(&self, allowance_id: &str) -> Result<Allowance, AbacusError> {
        let mut l = self.inner.lock().unwrap();
        l.calls.push(Call::GetAllowance {
            allowance_id: allowance_id.to_string(),
        });
        let a = l
            .allowances
            .get(allowance_id)
            .cloned()
            .ok_or(AbacusError::AllowanceNotFound)?;
        Ok(Allowance {
            allowance_id: allowance_id.to_string(),
            limit_micros: a.limit,
            spent_micros: a.spent,
            revoked: a.revoked,
        })
    }

    async fn spend_allowance(
        &self,
        req: AllowanceSpend,
    ) -> Result<AllowanceSpendReceipt, AbacusError> {
        let mut l = self.inner.lock().unwrap();
        l.calls.push(Call::SpendAllowance {
            allowance_id: req.allowance_id.clone(),
            amount: req.amount_micros,
        });

        if let Some(existing) = l.by_idem.get(&req.idem_key).cloned() {
            let remaining = l
                .allowances
                .get(&req.allowance_id)
                .map(|a| (a.limit - a.spent).max(0))
                .unwrap_or(0);
            return Ok(AllowanceSpendReceipt {
                hold_id: Some(existing),
                remaining_micros: remaining,
                settlement_seq: 0, // replay: no new settlement
            });
        }

        let a = l
            .allowances
            .get(&req.allowance_id)
            .cloned()
            .ok_or(AbacusError::AllowanceNotFound)?;
        if a.revoked {
            return Err(AbacusError::AllowanceNotFound);
        }
        // Atomic ceiling check: est_max must fit under the remaining headroom.
        let remaining = (a.limit - a.spent).max(0);
        if req.amount_micros > remaining {
            return Err(AbacusError::AllowanceExceeded);
        }
        // Reserve against the ceiling: `spent` rises now, restored on release,
        // finalized on capture (see capture/release).
        if let Some(am) = l.allowances.get_mut(&req.allowance_id) {
            am.spent += req.amount_micros;
        }
        let hold_id = Self::next_hold_id(&mut l);
        l.holds.insert(
            hold_id.clone(),
            HoldRec {
                account: format!("allowance:{}", req.allowance_id),
                amount: req.amount_micros,
                settled: false,
                allowance_id: Some(req.allowance_id.clone()),
            },
        );
        l.by_idem.insert(req.idem_key.clone(), hold_id.clone());
        let new_remaining = (a.limit - (a.spent + req.amount_micros)).max(0);
        l.seq += 1;
        let seq = l.seq;
        Ok(AllowanceSpendReceipt {
            hold_id: Some(hold_id),
            remaining_micros: new_remaining,
            settlement_seq: seq,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hold_reserves_then_capture_returns_remainder() {
        let mint = LedgerFakeMint::new().with_balance("acct_a", 10_000);
        let hold = mint
            .create_hold(Hold {
                account_id: "acct_a".into(),
                amount_micros: 6_000,
                idem_key: "r1".into(),
            })
            .await
            .unwrap();
        assert_eq!(mint.balance("acct_a"), 4_000);
        mint.capture_hold(
            &hold.hold_id,
            Capture {
                amount_micros: 2_000,
                payee: "ait1qpayee".into(),
                idem_key: "r1".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(mint.balance("acct_a"), 8_000);
    }

    #[tokio::test]
    async fn concurrent_holds_cannot_overshoot_ceiling() {
        use std::sync::Arc;
        // Balance covers exactly ONE 700 hold; a second must 402.
        let mint = Arc::new(LedgerFakeMint::new().with_balance("acct_a", 1_000));
        let m1 = mint.clone();
        let m2 = mint.clone();
        let h1 = tokio::spawn(async move {
            m1.create_hold(Hold {
                account_id: "acct_a".into(),
                amount_micros: 700,
                idem_key: "a".into(),
            })
            .await
        });
        let h2 = tokio::spawn(async move {
            m2.create_hold(Hold {
                account_id: "acct_a".into(),
                amount_micros: 700,
                idem_key: "b".into(),
            })
            .await
        });
        let (r1, r2) = (h1.await.unwrap(), h2.await.unwrap());
        let successes = [r1.is_ok(), r2.is_ok()].iter().filter(|&&b| b).count();
        assert_eq!(successes, 1, "only one hold may reserve the single slot");
        assert!(mint.balance("acct_a") >= 0);
    }

    #[tokio::test]
    async fn allowance_spend_over_ceiling_is_402() {
        let mint = LedgerFakeMint::new().with_allowance("alw_x", 1_000, 900);
        let err = mint
            .spend_allowance(AllowanceSpend {
                allowance_id: "alw_x".into(),
                amount_micros: 500,
                payee: "ait1qpayee".into(),
                idem_key: "s1".into(),
            })
            .await
            .unwrap_err();
        assert_eq!(err, AbacusError::AllowanceExceeded);
    }

    // Reserve → capture on an allowance draws the ceiling down by the CAPTURED
    // amount only — never the reserved ceiling. The reservation raised `spent` by
    // the full reserve; capture must return the unused remainder so `remaining`
    // reflects just the actual spend (regression against the double-count that left
    // both the reservation and the capture drawn down).
    #[tokio::test]
    async fn allowance_capture_draws_down_only_the_actual() {
        let mint = LedgerFakeMint::new().with_allowance("alw_x", 10_000, 0);
        let spend = mint
            .spend_allowance(AllowanceSpend {
                allowance_id: "alw_x".into(),
                amount_micros: 1_200,
                payee: "ait1qpayee".into(),
                idem_key: "s1".into(),
            })
            .await
            .unwrap();
        // Reserved the full ceiling slice up front.
        assert_eq!(mint.allowance_remaining("alw_x"), 8_800);

        mint.capture_hold(
            spend.hold_id.as_deref().unwrap(),
            Capture {
                amount_micros: 750,
                payee: "ait1qpayee".into(),
                idem_key: "s1".into(),
            },
        )
        .await
        .unwrap();

        // Only the 750 actual stays drawn down: 10_000 - 750 = 9_250.
        assert_eq!(mint.allowance_remaining("alw_x"), 9_250);
    }

    // Releasing an allowance reservation restores the ceiling in full — no spend.
    #[tokio::test]
    async fn allowance_release_restores_full_ceiling() {
        let mint = LedgerFakeMint::new().with_allowance("alw_x", 10_000, 0);
        let spend = mint
            .spend_allowance(AllowanceSpend {
                allowance_id: "alw_x".into(),
                amount_micros: 1_200,
                payee: "ait1qpayee".into(),
                idem_key: "s1".into(),
            })
            .await
            .unwrap();
        assert_eq!(mint.allowance_remaining("alw_x"), 8_800);

        mint.release_hold(
            spend.hold_id.as_deref().unwrap(),
            Release {
                idem_key: "s1".into(),
            },
        )
        .await
        .unwrap();

        assert_eq!(mint.allowance_remaining("alw_x"), 10_000);
    }

    #[tokio::test]
    async fn allowance_spend_within_ceiling_yields_hold() {
        let mint = LedgerFakeMint::new().with_allowance("alw_x", 1_000, 0);
        let r = mint
            .spend_allowance(AllowanceSpend {
                allowance_id: "alw_x".into(),
                amount_micros: 400,
                payee: "ait1qpayee".into(),
                idem_key: "s1".into(),
            })
            .await
            .unwrap();
        assert!(r.hold_id.is_some());
        assert_eq!(r.remaining_micros, 600);
    }
}
