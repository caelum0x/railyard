//! The reserve gate: identity + priced estimate → [`EnforcementDecision`].
//!
//! This is where a looping agent is stopped. Given the resolved [`AgentIdentity`]
//! and the priced [`CostEstimate`], the gate opens a Mint reservation for
//! `est_max` — a DIRECT hold against the treasury, or a hold-backed spend against
//! a delegated allowance ceiling — and returns [`EnforcementDecision::Allow`] with
//! the opened `hold_id`. If Mint refuses (allowance exceeded / insufficient funds
//! / frozen), or a local per-request ceiling is breached, it returns
//! [`EnforcementDecision::Kill`] and NO upstream request is ever made.

use abacus_core::decision::EnforcementDecision;
use abacus_core::error::AbacusError;
use abacus_core::estimate::CostEstimate;
use abacus_core::identity::{AgentIdentity, FundingPath};
use abacus_core::transport::{AllowanceSpend, Hold, MintTransport};

/// The reservation that backs an [`EnforcementDecision::Allow`]. The `hold_id` is
/// threaded to settlement; `reserved_micros` is the `est_max` we held.
#[derive(Debug, Clone)]
pub struct Reservation {
    /// The opened Mint hold id.
    pub hold_id: String,
    /// Amount reserved (`est_max`), in µ$.
    pub reserved_micros: i64,
}

/// Run the reserve gate. Returns `Ok((decision, Some(reservation)))` on allow, or
/// `Ok((Kill, None))` when the budget refuses the request.
pub async fn enforce(
    mint: &dyn MintTransport,
    identity: &AgentIdentity,
    estimate: &CostEstimate,
    idem_key: &str,
) -> Result<(EnforcementDecision, Option<Reservation>), AbacusError> {
    let est_max = estimate.reserve_amount().get();

    // Defence-in-depth: a single request over the local per-request ceiling is
    // killed before any Mint call.
    if let Some(cap) = identity.budget.per_request_max_micros {
        if est_max > cap {
            let err = AbacusError::AllowanceExceeded;
            return Ok((EnforcementDecision::kill(&err), None));
        }
    }

    let reservation = match identity.budget.funding_path {
        FundingPath::DirectHold => {
            match mint
                .create_hold(Hold {
                    account_id: identity.funding_account().as_str().to_string(),
                    amount_micros: est_max,
                    idem_key: idem_key.to_string(),
                })
                .await
            {
                Ok(h) => Reservation {
                    hold_id: h.hold_id,
                    reserved_micros: h.amount_micros,
                },
                Err(e) if is_budget_refusal(&e) => {
                    return Ok((EnforcementDecision::kill(&e), None))
                }
                Err(e) => return Err(e),
            }
        }
        FundingPath::AllowanceCeiling => {
            let allowance_id =
                identity.budget.allowance_id.clone().ok_or_else(|| {
                    AbacusError::Config("allowance path without allowance_id".into())
                })?;
            match mint
                .spend_allowance(AllowanceSpend {
                    allowance_id,
                    amount_micros: est_max,
                    payee: identity.payee.as_str().to_string(),
                    idem_key: idem_key.to_string(),
                })
                .await
            {
                Ok(r) => {
                    let hold_id = r.hold_id.ok_or_else(|| AbacusError::Upstream {
                        status: 502,
                        code: "missing_hold".into(),
                        message: "allowance spend returned no hold_id".into(),
                    })?;
                    Reservation {
                        hold_id,
                        reserved_micros: est_max,
                    }
                }
                Err(e) if is_budget_refusal(&e) => {
                    return Ok((EnforcementDecision::kill(&e), None))
                }
                Err(e) => return Err(e),
            }
        }
    };

    Ok((
        EnforcementDecision::allow(reservation.hold_id.clone()),
        Some(reservation),
    ))
}

/// `true` for the errors that mean "the budget said no" — a clean kill, not a 5xx.
fn is_budget_refusal(e: &AbacusError) -> bool {
    matches!(
        e,
        AbacusError::AllowanceExceeded
            | AbacusError::InsufficientFunds
            | AbacusError::AccountFrozen
            | AbacusError::PayoutLimitExceeded
            | AbacusError::AllowanceNotFound
            | AbacusError::AccountNotFound
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_refusals_are_classified() {
        assert!(is_budget_refusal(&AbacusError::AllowanceExceeded));
        assert!(is_budget_refusal(&AbacusError::InsufficientFunds));
        assert!(is_budget_refusal(&AbacusError::AccountFrozen));
        assert!(!is_budget_refusal(&AbacusError::Transport("x".into())));
    }
}
