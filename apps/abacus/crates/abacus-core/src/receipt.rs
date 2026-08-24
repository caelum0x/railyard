//! The Mint receipt VIEW + the receipt persistence port.
//!
//! Every enforced request that captures produces a [`MintReceipt`] — the
//! system-of-record for agent spend. [`ReceiptStore`] is the async port that
//! persists it: fake (in-memory, asserted in tests) or real (Neon) in prod.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::AbacusError;
use crate::ids::{AgentId, BudgetId, RunId};

/// How a receipted request terminated on the money path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptOutcome {
    /// 2xx-with-usage: the hold was captured for the exact actual cost.
    Captured,
    /// Non-2xx / error / timeout: the hold was released, agent charged nothing.
    Released,
}

/// The system-of-record view of one enforced request's spend.
///
/// A `Captured` receipt anchors `captured_micros` + `settlement_seq` to the Mint
/// ledger; a `Released` receipt records that the reservation was returned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintReceipt {
    /// Spending agent.
    pub agent_id: AgentId,
    /// Run/session the spend belongs to.
    pub run_id: RunId,
    /// Budget the spend drew against.
    pub budget_id: BudgetId,
    /// The Mint hold that backed the reservation.
    pub hold_id: String,
    /// Amount reserved on the hold (`est_max`), in µ$.
    pub reserved_micros: i64,
    /// Amount actually captured, in µ$ (`0` when released).
    pub captured_micros: i64,
    /// Payer balance after settlement, in µ$.
    pub balance_after_micros: i64,
    /// Ledger settlement sequence (receipt anchor); `0` when released.
    pub settlement_seq: u64,
    /// Terminal outcome.
    pub outcome: ReceiptOutcome,
}

/// The receipt system-of-record. Object-safe for `Arc<dyn ReceiptStore>`.
#[async_trait]
pub trait ReceiptStore: Send + Sync {
    /// Persist a settled receipt. Must be idempotent on `hold_id` so a retried
    /// settle does not double-write.
    async fn record(&self, receipt: &MintReceipt) -> Result<(), AbacusError>;

    /// Fetch a receipt by its backing hold id, if present.
    async fn get(&self, hold_id: &str) -> Result<Option<MintReceipt>, AbacusError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_serializes_captured_outcome() {
        let r = MintReceipt {
            agent_id: AgentId::generate(),
            run_id: RunId::generate(),
            budget_id: BudgetId::generate(),
            hold_id: "hold_1".into(),
            reserved_micros: 5_000,
            captured_micros: 3_200,
            balance_after_micros: 96_800,
            settlement_seq: 42,
            outcome: ReceiptOutcome::Captured,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains(r#""outcome":"captured""#));
        assert!(json.contains(r#""captured_micros":3200"#));
    }
}
