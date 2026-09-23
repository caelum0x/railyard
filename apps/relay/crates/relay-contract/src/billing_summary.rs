//! Aggregated billing summary contract for the Relay billing dashboard.
//!
//! This is the transport shape returned by the gateway
//! `GET /relay/billing/summary` endpoint. It is computed by rolling up the
//! durable `relay_cost_events` outbox and `relay_settlement_evidence` tables
//! (see `relay-storage-postgres`) for a single authenticated principal, so the
//! summary never surfaces another tenant's spend.

use serde::{Deserialize, Serialize};
use ts_rs::TS;
use uuid::Uuid;

use crate::money::Microdollars;

/// Wallet-level rollup for the authenticated principal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct WalletSummary {
    /// Total exact amount charged across every settled request (micro-USD).
    pub total_charged_micros: Microdollars,
    /// Total amount authorized (held) across every settled request (micro-USD).
    pub total_authorized_micros: Microdollars,
    /// Total amount backed by signed settlement evidence (micro-USD).
    pub total_settled_micros: Microdollars,
    /// Number of durable cost events (one per terminal charge).
    pub cost_event_count: i64,
    /// Number of signed settlement receipts on file.
    pub settlement_count: i64,
    /// Epoch-millisecond timestamp of the most recent settlement, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub last_settled_at_ms: Option<i64>,
}

/// Spend and token totals across all of the principal's cost events.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SpendTotals {
    pub charged_micros: Microdollars,
    pub authorized_micros: Microdollars,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub total_tokens: i64,
}

/// Per-API-key spend row. Each Relay account owns one key today, but the
/// breakdown generalizes to accounts that mint several.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct KeySpend {
    pub key: String,
    pub charged_micros: Microdollars,
    pub cost_event_count: i64,
}

/// Per-model spend row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ModelSpend {
    pub model: String,
    pub charged_micros: Microdollars,
    pub total_tokens: i64,
    pub cost_event_count: i64,
}

/// One signed settlement receipt, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SettlementRecord {
    pub request_id: Uuid,
    pub amount_micros: Microdollars,
    pub settled_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub verified_at_ms: Option<i64>,
}

/// Full billing summary for one authenticated principal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct BillingSummary {
    pub principal: String,
    pub wallet: WalletSummary,
    pub totals: SpendTotals,
    pub by_key: Vec<KeySpend>,
    pub by_model: Vec<ModelSpend>,
    pub recent_settlements: Vec<SettlementRecord>,
}

/// One settlement receipt with full evidence detail, newest first. Returned by
/// `GET /relay/billing/settlements` for the settlement-history drill-down.
///
/// The immutable signed receipt is carried verbatim as a JSON string
/// (`receipt_json`) so the dashboard can render the exact evidence a caller can
/// later present as proof of payment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SettlementDetail {
    pub request_id: Uuid,
    pub amount_micros: Microdollars,
    pub settled_at_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub verified_at_ms: Option<i64>,
    /// Whether a signed attestation is stored alongside the receipt.
    pub has_attestation: bool,
    /// The immutable signed settlement receipt, serialized as a JSON string.
    pub receipt_json: String,
}

/// The principal's settlement-evidence detail list, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct SettlementHistory {
    pub principal: String,
    pub settlements: Vec<SettlementDetail>,
    /// Cursor for the next (older) page; absent on the last page. Pass it
    /// back as `?before=` to continue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub next_before: Option<String>,
}

/// One `(key, day-bucket)` spend point for the per-API-key spend drill-down.
///
/// Buckets are UTC calendar days derived from each cost event's `created_at`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct KeySpendPoint {
    pub key: String,
    /// UTC day-bucket start as epoch milliseconds.
    pub bucket_start_ms: i64,
    pub charged_micros: Microdollars,
    pub total_tokens: i64,
    pub cost_event_count: i64,
}

/// Per-API-key spend over time for the authenticated principal. Returned by
/// `GET /relay/billing/keys`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct KeySpendSeries {
    pub principal: String,
    pub points: Vec<KeySpendPoint>,
}
