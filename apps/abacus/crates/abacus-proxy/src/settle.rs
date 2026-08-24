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
    guard: &mut HoldGuard,
    terminal: Terminal,
) -> Result<SettleOutcome, AbacusError> {
    let (outcome, receipt_outcome, captured, balance_after, seq) = match terminal {
        Terminal::Capture { actual_micros } => {
            let cap = mint
                .capture_hold(
                    guard.hold_id(),
                    Capture {
                        amount_micros: actual_micros,
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
            )
        }
        Terminal::Release => {
            mint.release_hold(
                guard.hold_id(),
                Release {
                    idem_key: guard.idem_key().to_string(),
                },
            )
            .await?;
            (SettleOutcome::Released, ReceiptOutcome::Released, 0, 0, 0)
        }
    };

    let receipt = MintReceipt {
        agent_id: identity_agent_id(identity),
        run_id: identity.run_id.clone(),
        budget_id: identity.budget.budget_id.clone(),
        hold_id: guard.hold_id().to_string(),
        reserved_micros: guard.reserved_micros,
        captured_micros: captured,
        balance_after_micros: balance_after,
        settlement_seq: seq,
        outcome: receipt_outcome,
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

    #[test]
    fn dropping_disarmed_guard_is_quiet() {
        let mut g = HoldGuard::new("hold_x", "idem_x", 100);
        g.disarm();
        drop(g);
    }
}
