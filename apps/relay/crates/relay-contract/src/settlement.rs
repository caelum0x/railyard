//! Authenticated settlement-evidence response contract.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::money::Microdollars;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SettlementReceiptProof {
    #[serde(alias = "seq")]
    pub sequence: u64,
    #[serde(alias = "op")]
    pub operation: String,
    #[serde(alias = "op_hash")]
    pub operation_hash: String,
    pub amount_micros: Microdollars,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub balance_after_micros: Option<Microdollars>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub prev_hash: Option<String>,
    #[serde(alias = "hash")]
    pub receipt_hash: String,
    #[serde(alias = "sig")]
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
pub struct SettlementEvidence {
    pub request_id: Uuid,
    pub principal: String,
    pub receipt: SettlementReceiptProof,
    /// Mint-signed attestation. Relay verifies it before persistence; clients
    /// that need independent verification can pass it to `relay-aitp`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "unknown")]
    pub attestation: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub verified_at_ms: Option<i64>,
    pub settled_at_ms: i64,
}

impl SettlementEvidence {
    pub fn captured_micros(&self) -> Microdollars {
        self.receipt.amount_micros
    }
}
