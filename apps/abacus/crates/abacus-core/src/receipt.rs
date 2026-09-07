//! The Mint receipt VIEW + the receipt persistence port.
//!
//! Every enforced request that captures produces a [`MintReceipt`] — the
//! system-of-record for agent spend. [`ReceiptStore`] is the async port that
//! persists it: fake (in-memory, asserted in tests) or real (Neon) in prod.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::AbacusError;
use crate::ids::{AgentId, BudgetId, RunId};
use crate::transport::SettlementProof;

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
    /// Principal whose Mint balance funded the request.
    pub payer_account: String,
    /// Stable request correlation key shared with Relay/Mint/Ledgerline.
    pub request_id: String,
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
    /// Mint's signed, hash-linked evidence for capture or release.
    pub settlement_proof: SettlementProof,
}

/// The receipt system-of-record. Object-safe for `Arc<dyn ReceiptStore>`.
#[async_trait]
pub trait ReceiptStore: Send + Sync {
    /// Persist a settled receipt. Must be idempotent on `hold_id` so a retried
    /// settle does not double-write.
    async fn record(&self, receipt: &MintReceipt) -> Result<(), AbacusError>;

    /// Fetch a receipt by its backing hold id, if present.
    async fn get(&self, hold_id: &str) -> Result<Option<MintReceipt>, AbacusError>;

    /// Fetch a receipt by the stable cross-system `request_id` a caller supplies
    /// (the `x-request-id` correlation shared with Relay/Mint/Ledgerline). This
    /// is the audit key a client actually holds after a proxied request, so the
    /// receipt system-of-record can be queried without knowing the internal Mint
    /// hold id.
    async fn get_by_request_id(
        &self,
        request_id: &str,
    ) -> Result<Option<MintReceipt>, AbacusError>;
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
            payer_account: "acct_treasury".into(),
            request_id: "request_1".into(),
            hold_id: "hold_1".into(),
            reserved_micros: 5_000,
            captured_micros: 3_200,
            balance_after_micros: 96_800,
            settlement_seq: 42,
            outcome: ReceiptOutcome::Captured,
            settlement_proof: SettlementProof {
                sequence: 42,
                operation: "hold.capture".into(),
                operation_hash: "op_hash".into(),
                receipt_hash: "receipt_hash".into(),
                signature: "signature".into(),
            },
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains(r#""outcome":"captured""#));
        assert!(json.contains(r#""captured_micros":3200"#));
    }
}
