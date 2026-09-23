//! Agent identity + budget policy + the per-agent budget source port.
//!
//! [`AgentIdentity`] is the resolved answer to "who is spending, out of which
//! budget, into whose ledger accounts". [`BudgetPolicy`] is the enforcement knob
//! set (ceiling + funding path). [`BudgetRepo`] is the async port that maps an
//! authenticated agent+run to its identity/policy — fake in tests, real (Neon /
//! config) in prod.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::AbacusError;
use crate::ids::{AccountId, AgentId, Ait1Address, BudgetId, RunId};

/// Which Mint mechanism backs a request's reserve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FundingPath {
    /// Direct hold against the agent's treasury account (`POST /v1/holds`).
    DirectHold,
    /// Hold-backed spend against a delegated allowance ceiling
    /// (`POST /v1/allowances/:id/spend`).
    AllowanceCeiling,
}

/// The enforcement policy for one agent's budget.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetPolicy {
    /// The budget this policy governs.
    pub budget_id: BudgetId,
    /// Which Mint mechanism reserves against the budget.
    pub funding_path: FundingPath,
    /// The delegated allowance id, required when `funding_path` is
    /// [`FundingPath::AllowanceCeiling`].
    pub allowance_id: Option<String>,
    /// A local hard ceiling in µ$ (defence-in-depth atop the Mint ceiling); a
    /// single request whose `est_max` exceeds this is killed before any Mint call.
    pub per_request_max_micros: Option<i64>,
}

/// The resolved spend identity for one enforced request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentIdentity {
    /// The Mint account representing the agent itself.
    pub agent_acct: AccountId,
    /// The run/session this request belongs to.
    pub run_id: RunId,
    /// The treasury account the direct-hold path reserves against.
    pub treasury: AccountId,
    /// The provider payee (`ait1…`) the eventual capture settles to, resolved to
    /// an `AccountId` via the Mint address lookup at capture time.
    pub payee: Ait1Address,
    /// The budget policy governing this agent's spend.
    pub budget: BudgetPolicy,
}

impl AgentIdentity {
    /// The account a direct hold reserves against (the treasury).
    pub fn funding_account(&self) -> &AccountId {
        &self.treasury
    }
}

/// The per-agent budget/policy source. Object-safe for `Arc<dyn BudgetRepo>`.
///
/// Fake in tests (seeded fixtures); real in prod (config / Neon lookup). The
/// proxy calls this once per request to resolve who is spending and under which
/// ceiling before it ever touches Mint.
#[async_trait]
pub trait BudgetRepo: Send + Sync {
    /// Resolve the spend identity + policy for an authenticated agent + run.
    async fn resolve(&self, agent: &AgentId, run: &RunId) -> Result<AgentIdentity, AbacusError>;

    /// Every configured agent and its policy (operator console / budget views).
    /// Stores that cannot enumerate agents report a storage error rather than
    /// pretending the fleet is empty.
    async fn list_agents(&self) -> Result<Vec<AgentProfile>, AbacusError> {
        Err(AbacusError::Storage(
            "this budget source cannot enumerate agents".to_string(),
        ))
    }
}

/// One configured agent as seen by operators (no run binding).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfile {
    /// The agent id.
    pub agent_id: AgentId,
    /// The Mint account representing the agent.
    pub agent_acct: AccountId,
    /// The treasury the direct-hold path reserves against.
    pub treasury: AccountId,
    /// The provider payee.
    pub payee: Ait1Address,
    /// The budget policy.
    pub budget: BudgetPolicy,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn funding_account_is_treasury() {
        let id = AgentIdentity {
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
        };
        assert_eq!(id.funding_account().as_str(), "acct_treasury01");
    }
}
