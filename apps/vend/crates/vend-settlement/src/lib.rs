//! # vend-settlement — explicit reserve → capture/release state machine.
//!
//! Ported in spirit from the Relay gateway's `billing.rs`. A [`HoldGuard`] models
//! the RAII window between opening a Mint hold and driving it to a terminal
//! state. The rule is asymmetric and conservative:
//!
//! - capture the rated charge derived from Relay's exact reported cost ONLY on a
//!   2xx-with-usage terminal,
//! - **release** on every other terminal: non-2xx, transport error, aborted
//!   stream, or a handler panic.
//!
//! Because async work cannot run in `Drop`, settlement is driven explicitly via
//! [`settle`], which disarms the guard; a guard dropped still-armed logs loudly
//! so an out-of-band reaper can release the orphaned hold. That reaper now ships
//! in [`reap`]: it releases a hold only when it is past a TTL, still pending,
//! and confirmed still-`Open` by Mint — closing the panic-orphaned-hold gap.

use async_trait::async_trait;

use vend_core::error::VendError;
use vend_core::ids::ChargeId;
use vend_core::money::Charge;
use vend_core::ports::{UsageOutcome, UsageRepo};
use vend_core::transport::{CaptureRequest, MintTransport, RelayResponse, ReleaseRequest};

pub mod payout;
pub mod reap;
pub use payout::{execute_payout, prepare_payout, PayoutConfig};
pub use reap::{
    classify_hold, past_ttl, reap_orphaned_hold, ReapAction, ReapCandidate, ReapOutcome,
};

/// What the request's terminal state tells settlement to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Terminal {
    /// 2xx with valid usage: capture this complete rated amount.
    Capture { actual: Charge },
    /// Anything else: release the hold, no charge.
    Release,
}

impl Terminal {
    /// Derive the terminal from a Relay response and the priced actual cost.
    /// Capture only when the upstream succeeded AND reported usage.
    pub fn from_relay(resp: &RelayResponse, actual: Charge) -> Terminal {
        if resp.is_success() && resp.usage.is_some() {
            Terminal::Capture { actual }
        } else {
            Terminal::Release
        }
    }
}

/// The result of driving a hold to completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettleOutcome {
    /// Captured this exact amount, in µ$.
    Captured(i64),
    /// Released; no charge.
    Released,
}

impl SettleOutcome {
    /// The corresponding persisted [`UsageOutcome`].
    pub fn usage_outcome(self) -> UsageOutcome {
        match self {
            SettleOutcome::Captured(_) => UsageOutcome::Captured,
            SettleOutcome::Released => UsageOutcome::Released,
        }
    }
}

/// RAII guard over the hold → terminal window. Arm it right after the hold is
/// opened; [`settle`] disarms it. A still-armed drop means the request path
/// panicked or forgot to settle — logged so a reaper can release the hold.
pub struct HoldGuard {
    hold_id: String,
    idem_key: String,
    armed: bool,
}

impl HoldGuard {
    /// Arm a guard for a freshly-opened hold.
    pub fn new(hold_id: impl Into<String>, idem_key: impl Into<String>) -> Self {
        HoldGuard {
            hold_id: hold_id.into(),
            idem_key: idem_key.into(),
            armed: true,
        }
    }

    /// The guarded Mint hold id.
    pub fn hold_id(&self) -> &str {
        &self.hold_id
    }

    /// Stable idempotency prefix for this charge.
    pub fn idem_key(&self) -> &str {
        &self.idem_key
    }

    fn capture_idem_key(&self) -> String {
        format!("{}:capture", self.idem_key)
    }

    fn release_idem_key(&self) -> String {
        format!("{}:release", self.idem_key)
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

/// Drive a hold to its terminal state, writing the `metered_usage` outcome.
///
/// Capture path: `capture_hold(actual)` then `mark_captured`. Release path:
/// `release_hold()` then `mark_released`. Each Mint operation gets a distinct
/// key derived from one stable charge prefix, matching Mint's global
/// idempotency namespace. Disarms `guard` after Mint reaches a terminal state.
pub async fn settle(
    mint: &dyn MintTransport,
    usage: &dyn UsageRepo,
    charge_id: &ChargeId,
    guard: &mut HoldGuard,
    settlement_account: &str,
    terminal: Terminal,
) -> Result<SettleOutcome, VendError> {
    let outcome = match terminal {
        Terminal::Capture { actual } => {
            let receipt = mint
                .capture_hold(
                    guard.hold_id(),
                    CaptureRequest {
                        amount_micros: actual.charge_micros,
                        to: settlement_account.to_string(),
                        idem_key: guard.capture_idem_key(),
                    },
                )
                .await?;
            if receipt.captured_micros != actual.charge_micros {
                guard.disarm();
                return Err(VendError::Transport(
                    "Mint capture receipt amount did not match request".to_string(),
                ));
            }
            // Mint is terminal even if the local projection update fails. A
            // remaining Pending row can be reconciled without re-capturing.
            guard.disarm();
            usage.mark_captured(charge_id, &actual).await?;
            SettleOutcome::Captured(actual.charge_micros)
        }
        Terminal::Release => {
            mint.release_hold(
                guard.hold_id(),
                ReleaseRequest {
                    idem_key: guard.release_idem_key(),
                },
            )
            .await?;
            guard.disarm();
            usage.mark_released(charge_id).await?;
            SettleOutcome::Released
        }
    };
    Ok(outcome)
}

/// A trait alias documenting the full settlement dependency surface, so a caller
/// can require both seams in one bound.
#[async_trait]
pub trait Settler: Send + Sync {
    /// Open→terminal in one call given an already-opened hold.
    async fn drive(
        &self,
        charge_id: &ChargeId,
        guard: &mut HoldGuard,
        terminal: Terminal,
    ) -> Result<SettleOutcome, VendError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use vend_core::transport::{RelayResponse, Usage};

    fn resp(status: u16, usage: Option<Usage>) -> RelayResponse {
        RelayResponse {
            status,
            body: serde_json::Value::Null,
            usage,
        }
    }

    fn charge(amount: i64) -> Charge {
        Charge {
            provider_cost_micros: amount,
            markup_micros: 0,
            charge_micros: amount,
            platform_fee_micros: 0,
            tenant_net_micros: 0,
        }
    }

    #[test]
    fn success_with_usage_captures_actual() {
        let actual = charge(4_200);
        let t = Terminal::from_relay(&resp(200, Some(Usage::default())), actual);
        assert_eq!(t, Terminal::Capture { actual });
    }

    #[test]
    fn non_2xx_releases() {
        assert_eq!(
            Terminal::from_relay(&resp(500, Some(Usage::default())), charge(9)),
            Terminal::Release
        );
    }

    #[test]
    fn success_without_usage_releases() {
        assert_eq!(
            Terminal::from_relay(&resp(200, None), charge(9)),
            Terminal::Release
        );
    }

    #[test]
    fn dropping_disarmed_guard_is_quiet() {
        let mut g = HoldGuard::new("hold_x", "idem_x");
        g.disarm();
        drop(g);
    }
}
