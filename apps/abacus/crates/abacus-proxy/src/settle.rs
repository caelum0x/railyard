//! [`HoldGuard`] RAII + [`settle`] — the success-only settlement state machine.
//!
//! The rule (ADR-0002) is asymmetric and conservative:
//!
//! - capture the **EXACT actual** cost (never the estimate) ONLY on a
//!   2xx-with-usage terminal;
//! - **release** on every other terminal — non-2xx, transport error/timeout,
//!   aborted stream, or a handler panic.
//!
//! Because async work cannot run in `Drop`, settlement is driven explicitly via
//! [`settle`], which disarms the guard. A guard dropped still-armed logs loudly so
//! an out-of-band reaper can release the orphaned hold. Every terminal writes a
//! [`MintReceipt`] to the [`ReceiptStore`] — the system-of-record for agent spend.

use abacus_core::error::AbacusError;
use abacus_core::identity::AgentIdentity;
use abacus_core::receipt::{MintReceipt, ReceiptOutcome, ReceiptStore};
use abacus_core::transport::{Capture, MintTransport, Release};

/// What the request's terminal state tells settlement to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminal {
    /// 2xx with usage: capture this exact actual amount, in µ$.
    Capture { actual_micros: i64 },
    /// Anything else: release the hold, no charge.
    Release,
}

/// The result of driving a hold to completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettleOutcome {
    /// Captured this exact amount, in µ$.
    Captured(i64),
    /// Released; no charge.
    Released,
}

/// RAII guard over the hold → terminal window. Arm it right after the hold opens;
/// [`settle`] disarms it. A still-armed drop means the request path panicked or
/// forgot to settle — logged so a reaper can release the hold.
pub struct HoldGuard {
    hold_id: String,
    idem_key: String,
    reserved_micros: i64,
    armed: bool,
}

impl HoldGuard {
    /// Arm a guard for a freshly-opened hold reserving `reserved_micros`.
    pub fn new(
        hold_id: impl Into<String>,
        idem_key: impl Into<String>,
        reserved_micros: i64,
    ) -> Self {
        HoldGuard {
            hold_id: hold_id.into(),
            idem_key: idem_key.into(),
            reserved_micros,
            armed: true,
        }
    }

    /// The guarded Mint hold id.
    pub fn hold_id(&self) -> &str {
        &self.hold_id
    }

    /// The deterministic idem base reused across the hold triple.
    pub fn idem_key(&self) -> &str {
        &self.idem_key
    }

    /// Mark the hold as settled so `Drop` stays quiet.
    pub fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for HoldGuard {
    fn drop(&mut self) {
        if self.armed {
            tracing::error!(
                hold_id = %self.hold_id,
                "HoldGuard dropped while still armed — hold not settled; a reaper must release it"
            );
        }
    }
}

/// Drive a hold to its terminal state, then persist the [`MintReceipt`].
///
/// Capture path: `capture_hold(actual)` (settling to the identity's payee) →
/// record a `Captured` receipt. Release path: `release_hold()` → record a
/// `Released` receipt. Both reuse the deterministic `idem_key`, so a retried
/// terminal is a no-op on Mint. Disarms `guard`.
pub async fn settle(
    mint: &dyn MintTransport,
    receipts: &dyn ReceiptStore,
    identity: &AgentIdentity,
    request_id: &str,
    guard: &mut HoldGuard,
    terminal: Terminal,
) -> Result<SettleOutcome, AbacusError> {
    let (outcome, receipt_outcome, captured, balance_after, seq, proof) = match terminal {
        Terminal::Capture { actual_micros } => {
            // Clamp the capture to the reserved hold. Mint rejects any capture
            // that exceeds the held amount with `CaptureExceedsHold`; letting
            // that error escape here would abort settlement BEFORE the guard is
            // disarmed, stranding the hold armed and billing the agent NOTHING
            // for a successful call (revenue leakage — the reaper later releases
            // it for free). The priced `est_max` is a conservative ceiling, but
            // the prompt-token estimate (a chars/4 heuristic) and the default
            // completion ceiling can under-count real usage, so `actual` can
            // legitimately land above the reserve. Bill up to the reserved
            // amount — never more (it is physically held), never zero.
            let capture_micros = actual_micros.min(guard.reserved_micros);
            if capture_micros < actual_micros {
                tracing::warn!(
                    hold_id = %guard.hold_id(),
                    actual_micros,
                    reserved_micros = guard.reserved_micros,
                    "actual cost exceeded the reserved hold; capturing the reserved ceiling (under-reservation — revisit pricing)"
                );
            }
            let cap = mint
                .capture_hold(
                    guard.hold_id(),
                    Capture {
                        amount_micros: capture_micros,
                        payee: identity.payee.as_str().to_string(),
                        idem_key: guard.idem_key().to_string(),
                    },
                )
                .await?;
            (
                SettleOutcome::Captured(cap.captured_micros),
                ReceiptOutcome::Captured,
                cap.captured_micros,
                cap.balance_after_micros,
                cap.settlement_seq,
                cap.proof,
            )
        }
        Terminal::Release => {
            let release = mint
                .release_hold(
                    guard.hold_id(),
                    Release {
                        idem_key: guard.idem_key().to_string(),
                    },
                )
                .await?;
            (
                SettleOutcome::Released,
                ReceiptOutcome::Released,
                0,
                0,
                release.proof.sequence,
                release.proof,
            )
        }
    };

    let receipt = MintReceipt {
        agent_id: identity_agent_id(identity),
        run_id: identity.run_id.clone(),
        budget_id: identity.budget.budget_id.clone(),
        payer_account: identity.treasury.as_str().to_string(),
        request_id: request_id.to_string(),
        hold_id: guard.hold_id().to_string(),
        reserved_micros: guard.reserved_micros,
        captured_micros: captured,
        balance_after_micros: balance_after,
        settlement_seq: seq,
        outcome: receipt_outcome,
        settlement_proof: proof,
    };
    receipts.record(&receipt).await?;

    guard.disarm();
    Ok(outcome)
}

/// The agent id anchoring the receipt. Derived from the identity's agent account
/// so the receipt is attributable even though `AgentIdentity` stores the account
/// form; the run/budget already fully scope the spend.
fn identity_agent_id(identity: &AgentIdentity) -> abacus_core::ids::AgentId {
    // The agent account carries the same ULID body under a different prefix; if it
    // cannot be reinterpreted we fall back to a fresh, still-unique anchor.
    let body = identity
        .agent_acct
        .as_str()
        .strip_prefix(abacus_core::ids::AccountId::PREFIX)
        .unwrap_or_default();
    abacus_core::ids::AgentId::parse(&format!("agent_{body}"))
        .unwrap_or_else(|_| abacus_core::ids::AgentId::generate())
}

#[cfg(test)]
mod tests {
    use super::*;

    use abacus_core::identity::{BudgetPolicy, FundingPath};
    use abacus_core::ids::{AccountId, Ait1Address, BudgetId, RunId};
    use abacus_core::transport::Hold;
    use abacus_testkit::fake_mint::Call;
    use abacus_testkit::{FakeReceiptStore, LedgerFakeMint};

    #[test]
    fn dropping_disarmed_guard_is_quiet() {
        let mut g = HoldGuard::new("hold_x", "idem_x", 100);
        g.disarm();
        drop(g);
    }

    fn identity() -> AgentIdentity {
        AgentIdentity {
            agent_acct: AccountId::parse("acct_agent01").unwrap(),
            run_id: RunId::generate(),
            treasury: AccountId::parse("acct_treasury01").unwrap(),
            payee: Ait1Address::parse("ait1qprovider").unwrap(),
            budget: BudgetPolicy {
                budget_id: BudgetId::generate(),
                funding_path: FundingPath::DirectHold,
                allowance_id: None,
                per_request_max_micros: Some(5_000_000),
            },
        }
    }

    /// Open a real hold reserving `reserved` µ$ against a seeded treasury, then
    /// return the guard armed over it (mirrors the handler's arming step).
    async fn armed_hold(mint: &LedgerFakeMint, reserved: i64) -> HoldGuard {
        let receipt = mint
            .create_hold(Hold {
                account_id: "acct_treasury01".into(),
                amount_micros: reserved,
                idem_key: "idem_base".into(),
            })
            .await
            .unwrap();
        HoldGuard::new(receipt.hold_id, "idem_base", reserved)
    }

    // The regression: a successful call whose ACTUAL cost exceeds the reserved
    // hold must still settle. Before the clamp, `capture_hold(actual)` returned
    // `CaptureExceedsHold`, aborting settlement before disarm — the hold stayed
    // armed and the agent was billed nothing for a successful call.
    #[tokio::test]
    async fn capture_over_reserve_bills_the_ceiling_not_zero() {
        let mint = LedgerFakeMint::new().with_balance("acct_treasury01", 10_000);
        let receipts = FakeReceiptStore::new();
        let id = identity();
        let mut guard = armed_hold(&mint, 4_000).await;

        // Actual came in ABOVE the reserved 4,000 (under-estimated prompt).
        let outcome = settle(
            &mint,
            &receipts,
            &id,
            "request_over_reserve",
            &mut guard,
            Terminal::Capture {
                actual_micros: 6_500,
            },
        )
        .await
        .expect("settlement must not fail when actual exceeds the reserve");

        // Captured exactly the reserved ceiling — never more, never zero.
        assert_eq!(outcome, SettleOutcome::Captured(4_000));
        assert!(!guard.armed, "guard disarmed after a clamped capture");
        assert_eq!(receipts.len(), 1, "a receipt records the settled charge");

        // Mint saw a capture for the clamped amount (not the over-estimate).
        assert!(matches!(
            mint.calls().last().unwrap(),
            Call::Capture { amount, .. } if *amount == 4_000
        ));
        // Payer keeps only the un-reserved remainder (10,000 - 4,000 captured).
        assert_eq!(mint.balance("acct_treasury01"), 6_000);
    }

    // A capture at or below the reserve is unaffected: the exact actual settles
    // and the unused remainder returns to the payer.
    #[tokio::test]
    async fn capture_under_reserve_settles_exact_actual() {
        let mint = LedgerFakeMint::new().with_balance("acct_treasury01", 10_000);
        let receipts = FakeReceiptStore::new();
        let id = identity();
        let mut guard = armed_hold(&mint, 4_000).await;

        let outcome = settle(
            &mint,
            &receipts,
            &id,
            "request_under_reserve",
            &mut guard,
            Terminal::Capture {
                actual_micros: 2_500,
            },
        )
        .await
        .unwrap();

        assert_eq!(outcome, SettleOutcome::Captured(2_500));
        // Remainder (4,000 - 2,500) refunded: 6,000 + 1,500 back = 7,500.
        assert_eq!(mint.balance("acct_treasury01"), 7_500);
    }
}
