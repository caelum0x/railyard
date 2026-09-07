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

    use abacus_core::identity::BudgetPolicy;
    use abacus_core::ids::{AccountId, Ait1Address, BudgetId, RunId};
    use abacus_core::money::Micros;
    use abacus_testkit::fake_mint::Call;
    use abacus_testkit::LedgerFakeMint;

    #[test]
    fn budget_refusals_are_classified() {
        assert!(is_budget_refusal(&AbacusError::AllowanceExceeded));
        assert!(is_budget_refusal(&AbacusError::InsufficientFunds));
        assert!(is_budget_refusal(&AbacusError::AccountFrozen));
        assert!(!is_budget_refusal(&AbacusError::Transport("x".into())));
    }

    /// Build a direct-hold identity with an explicit local per-request ceiling.
    fn direct_identity(cap: Option<i64>) -> AgentIdentity {
        AgentIdentity {
            agent_acct: AccountId::parse("acct_agent01").unwrap(),
            run_id: RunId::generate(),
            treasury: AccountId::parse("acct_treasury01").unwrap(),
            payee: Ait1Address::parse("ait1qpayee").unwrap(),
            budget: BudgetPolicy {
                budget_id: BudgetId::generate(),
                funding_path: FundingPath::DirectHold,
                allowance_id: None,
                per_request_max_micros: cap,
            },
        }
    }

    /// Build an allowance-ceiling identity bound to `allowance_id`.
    fn allowance_identity(allowance_id: &str) -> AgentIdentity {
        AgentIdentity {
            agent_acct: AccountId::parse("acct_agent01").unwrap(),
            run_id: RunId::generate(),
            treasury: AccountId::parse("acct_treasury01").unwrap(),
            payee: Ait1Address::parse("ait1qpayee").unwrap(),
            budget: BudgetPolicy {
                budget_id: BudgetId::generate(),
                funding_path: FundingPath::AllowanceCeiling,
                allowance_id: Some(allowance_id.to_string()),
                per_request_max_micros: None,
            },
        }
    }

    /// A priced estimate reserving exactly `est_max` µ$.
    fn estimate(est_max: i64) -> CostEstimate {
        CostEstimate::new(Micros::new(1), Micros::new(est_max), 4, 100)
    }

    // The defence-in-depth local ceiling MUST fire before any Mint call: a request
    // priced above `per_request_max_micros` is killed with NO transport call at all
    // (a regression that moved the check after create_hold would show a CreateHold).
    #[tokio::test]
    async fn local_per_request_ceiling_kills_before_any_mint_call() {
        let mint = LedgerFakeMint::new().with_balance("acct_treasury01", 1_000_000);
        let id = direct_identity(Some(1_000));
        // est_max (5_000) exceeds the local 1_000 ceiling.
        let (decision, reservation) = enforce(&mint, &id, &estimate(5_000), "idem_local_cap")
            .await
            .unwrap();

        match decision {
            EnforcementDecision::Kill { reason, status } => {
                assert_eq!(reason, "allowance_exceeded");
                assert_eq!(status, 402);
            }
            other => panic!("expected a local-ceiling kill, got {other:?}"),
        }
        assert!(reservation.is_none());
        // The kill happened before Mint was ever touched.
        assert!(
            mint.calls().is_empty(),
            "no Mint call may precede a local-ceiling kill"
        );
        // Treasury spendable is untouched.
        assert_eq!(mint.balance("acct_treasury01"), 1_000_000);
    }

    // A request AT the local ceiling is allowed (the check is strictly greater-than).
    #[tokio::test]
    async fn direct_hold_allow_opens_a_hold_and_reserves_est_max() {
        let mint = LedgerFakeMint::new().with_balance("acct_treasury01", 1_000_000);
        let id = direct_identity(Some(5_000));
        let (decision, reservation) = enforce(&mint, &id, &estimate(5_000), "idem_direct_allow")
            .await
            .unwrap();

        assert!(decision.is_allowed());
        let reservation = reservation.expect("allow implies a reservation");
        assert_eq!(reservation.reserved_micros, 5_000);
        assert!(matches!(
            decision,
            EnforcementDecision::Allow { hold_id } if hold_id == reservation.hold_id
        ));
        // A single direct hold was opened for est_max against the treasury.
        assert!(matches!(
            mint.calls().as_slice(),
            [Call::CreateHold { account, amount }]
                if account == "acct_treasury01" && *amount == 5_000
        ));
        assert_eq!(mint.balance("acct_treasury01"), 1_000_000 - 5_000);
    }

    // The direct budget-refusal kill: an under-funded treasury 402s cleanly and
    // returns no reservation (InsufficientFunds is classified as a budget refusal).
    #[tokio::test]
    async fn direct_hold_insufficient_funds_kills() {
        let mint = LedgerFakeMint::new().with_balance("acct_treasury01", 100);
        let id = direct_identity(Some(5_000_000));
        let (decision, reservation) = enforce(&mint, &id, &estimate(5_000), "idem_direct_kill")
            .await
            .unwrap();

        match decision {
            EnforcementDecision::Kill { reason, status } => {
                assert_eq!(reason, "insufficient_funds");
                assert_eq!(status, 402);
            }
            other => panic!("expected an insufficient-funds kill, got {other:?}"),
        }
        assert!(reservation.is_none());
        // The reserve was attempted then refused; the balance never moved.
        assert!(matches!(mint.calls().as_slice(), [Call::CreateHold { .. }]));
        assert_eq!(mint.balance("acct_treasury01"), 100);
    }

    // The allowance-ceiling Allow branch: a hold-backed spend opens against the
    // delegated allowance and threads its hold_id into the reservation.
    #[tokio::test]
    async fn allowance_ceiling_allow_spends_against_the_delegated_allowance() {
        let mint = LedgerFakeMint::new().with_allowance("alw_x", 10_000, 0);
        let id = allowance_identity("alw_x");
        let (decision, reservation) = enforce(&mint, &id, &estimate(5_000), "idem_alw_allow")
            .await
            .unwrap();

        assert!(decision.is_allowed());
        let reservation = reservation.expect("allow implies a reservation");
        assert_eq!(reservation.reserved_micros, 5_000);
        // Exactly one spend_allowance for est_max against the bound allowance.
        assert!(matches!(
            mint.calls().as_slice(),
            [Call::SpendAllowance { allowance_id, amount }]
                if allowance_id == "alw_x" && *amount == 5_000
        ));
        // The reserve drew the ceiling down by est_max.
        assert_eq!(mint.allowance_remaining("alw_x"), 5_000);
    }

    // The allowance budget-refusal kill: an exhausted ceiling 402s with
    // AllowanceExceeded and opens no hold.
    #[tokio::test]
    async fn allowance_ceiling_exceeded_kills() {
        let mint = LedgerFakeMint::new().with_allowance("alw_x", 10_000, 9_000);
        let id = allowance_identity("alw_x");
        let (decision, reservation) = enforce(&mint, &id, &estimate(5_000), "idem_alw_kill")
            .await
            .unwrap();

        match decision {
            EnforcementDecision::Kill { reason, status } => {
                assert_eq!(reason, "allowance_exceeded");
                assert_eq!(status, 402);
            }
            other => panic!("expected an allowance-exceeded kill, got {other:?}"),
        }
        assert!(reservation.is_none());
        // The spend was attempted and refused; headroom is untouched.
        assert!(matches!(
            mint.calls().as_slice(),
            [Call::SpendAllowance { .. }]
        ));
        assert_eq!(mint.allowance_remaining("alw_x"), 1_000);
    }
}
