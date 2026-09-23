//! Seed agents/budgets + in-memory [`BudgetRepo`] and [`ReceiptStore`] fakes.
//!
//! `FakeBudgetRepo` maps a seeded `agent_id` to a resolved [`AgentIdentity`];
//! `FakeReceiptStore` records every persisted [`MintReceipt`] for assertions.
//! Mirrors `vend-testkit::fixtures` + `aitp stub.rs`.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use abacus_core::error::AbacusError;
use abacus_core::identity::{AgentIdentity, AgentProfile, BudgetPolicy, BudgetRepo, FundingPath};
use abacus_core::ids::{AccountId, AgentId, Ait1Address, BudgetId, RunId};
use abacus_core::receipt::{MintReceipt, ReceiptStore};

/// A seeded agent + its resolved identity, keyed by `agent_id` string.
pub struct FakeBudgetRepo {
    identities: Mutex<HashMap<String, AgentIdentity>>,
}

impl Default for FakeBudgetRepo {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeBudgetRepo {
    /// An empty repo.
    pub fn new() -> Self {
        FakeBudgetRepo {
            identities: Mutex::new(HashMap::new()),
        }
    }

    /// Seed a direct-hold agent against a treasury account (generous local
    /// per-request ceiling, so the Mint balance is the only binding constraint).
    pub fn with_direct_hold_agent(
        self,
        agent: &AgentId,
        treasury: &AccountId,
        payee: &Ait1Address,
    ) -> Self {
        self.with_capped_direct_hold_agent(agent, treasury, payee, Some(5_000_000))
    }

    /// Seed a direct-hold agent with an explicit local per-request ceiling
    /// (`per_request_max_micros`). Pass a small `cap` to exercise the
    /// defence-in-depth kill that must fire BEFORE any Mint call; pass `None` to
    /// disable the local ceiling entirely.
    pub fn with_capped_direct_hold_agent(
        self,
        agent: &AgentId,
        treasury: &AccountId,
        payee: &Ait1Address,
        cap: Option<i64>,
    ) -> Self {
        let identity = AgentIdentity {
            agent_acct: AccountId::parse(&format!("acct_{}", &agent.as_str()[6..]))
                .unwrap_or_else(|_| treasury.clone()),
            run_id: RunId::generate(),
            treasury: treasury.clone(),
            payee: payee.clone(),
            budget: BudgetPolicy {
                budget_id: BudgetId::generate(),
                funding_path: FundingPath::DirectHold,
                allowance_id: None,
                per_request_max_micros: cap,
            },
        };
        self.identities
            .lock()
            .unwrap()
            .insert(agent.as_str().to_string(), identity);
        self
    }

    /// Seed an allowance-ceiling agent bound to a delegated allowance id.
    pub fn with_allowance_agent(
        self,
        agent: &AgentId,
        treasury: &AccountId,
        payee: &Ait1Address,
        allowance_id: &str,
    ) -> Self {
        let identity = AgentIdentity {
            agent_acct: treasury.clone(),
            run_id: RunId::generate(),
            treasury: treasury.clone(),
            payee: payee.clone(),
            budget: BudgetPolicy {
                budget_id: BudgetId::generate(),
                funding_path: FundingPath::AllowanceCeiling,
                allowance_id: Some(allowance_id.to_string()),
                per_request_max_micros: None,
            },
        };
        self.identities
            .lock()
            .unwrap()
            .insert(agent.as_str().to_string(), identity);
        self
    }
}

#[async_trait]
impl BudgetRepo for FakeBudgetRepo {
    async fn resolve(&self, agent: &AgentId, run: &RunId) -> Result<AgentIdentity, AbacusError> {
        let mut identity = self
            .identities
            .lock()
            .unwrap()
            .get(agent.as_str())
            .cloned()
            .ok_or(AbacusError::AccountNotFound)?;
        // Bind the resolved identity to the caller's run.
        identity.run_id = run.clone();
        Ok(identity)
    }

    async fn list_agents(&self) -> Result<Vec<AgentProfile>, AbacusError> {
        let identities = self.identities.lock().unwrap();
        let mut out: Vec<AgentProfile> = identities
            .iter()
            .filter_map(|(agent, identity)| {
                Some(AgentProfile {
                    agent_id: AgentId::parse(agent).ok()?,
                    agent_acct: identity.agent_acct.clone(),
                    treasury: identity.treasury.clone(),
                    payee: identity.payee.clone(),
                    budget: identity.budget.clone(),
                })
            })
            .collect();
        out.sort_by(|a, b| a.agent_id.as_str().cmp(b.agent_id.as_str()));
        Ok(out)
    }
}

/// An in-memory receipt system-of-record; records every persisted receipt.
pub struct FakeReceiptStore {
    by_hold: Mutex<HashMap<String, MintReceipt>>,
}

impl Default for FakeReceiptStore {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeReceiptStore {
    /// An empty store.
    pub fn new() -> Self {
        FakeReceiptStore {
            by_hold: Mutex::new(HashMap::new()),
        }
    }

    /// How many receipts have been recorded.
    pub fn len(&self) -> usize {
        self.by_hold.lock().unwrap().len()
    }

    /// `true` iff no receipts have been recorded.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait]
impl ReceiptStore for FakeReceiptStore {
    async fn record(&self, receipt: &MintReceipt) -> Result<(), AbacusError> {
        // Idempotent on hold_id: a retried settle overwrites the same slot.
        self.by_hold
            .lock()
            .unwrap()
            .insert(receipt.hold_id.clone(), receipt.clone());
        Ok(())
    }

    async fn get(&self, hold_id: &str) -> Result<Option<MintReceipt>, AbacusError> {
        Ok(self.by_hold.lock().unwrap().get(hold_id).cloned())
    }

    async fn get_by_request_id(
        &self,
        request_id: &str,
    ) -> Result<Option<MintReceipt>, AbacusError> {
        Ok(self
            .by_hold
            .lock()
            .unwrap()
            .values()
            .find(|receipt| receipt.request_id == request_id)
            .cloned())
    }

    async fn list_recent(
        &self,
        agent_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MintReceipt>, AbacusError> {
        let mut out: Vec<MintReceipt> = self
            .by_hold
            .lock()
            .unwrap()
            .values()
            .filter(|r| agent_id.is_none_or(|a| r.agent_id.as_str() == a))
            .cloned()
            .collect();
        out.sort_by(|a, b| b.hold_id.cmp(&a.hold_id));
        out.truncate(limit);
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn budget_repo_resolves_seeded_agent() {
        let agent = AgentId::generate();
        let treasury = AccountId::parse("acct_treasury01").unwrap();
        let payee = Ait1Address::parse("ait1qprovider").unwrap();
        let repo = FakeBudgetRepo::new().with_direct_hold_agent(&agent, &treasury, &payee);
        let run = RunId::generate();
        let id = repo.resolve(&agent, &run).await.unwrap();
        assert_eq!(id.treasury.as_str(), "acct_treasury01");
        assert_eq!(id.run_id, run);
    }

    #[tokio::test]
    async fn receipt_store_records_and_reads_back() {
        let store = FakeReceiptStore::new();
        assert!(store.is_empty());
        let r = MintReceipt {
            agent_id: AgentId::generate(),
            run_id: RunId::generate(),
            budget_id: BudgetId::generate(),
            payer_account: "acct_treasury01".into(),
            request_id: "request_1".into(),
            hold_id: "hold_1".into(),
            reserved_micros: 5_000,
            captured_micros: 3_000,
            balance_after_micros: 97_000,
            settlement_seq: 7,
            outcome: abacus_core::receipt::ReceiptOutcome::Captured,
            settlement_proof: abacus_core::transport::SettlementProof {
                sequence: 7,
                operation: "hold.capture".into(),
                operation_hash: "op_hash".into(),
                receipt_hash: "receipt_hash".into(),
                signature: "signature".into(),
            },
        };
        store.record(&r).await.unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(store.get("hold_1").await.unwrap().unwrap(), r);
    }
}
