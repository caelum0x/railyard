//! Read-side / operator vocabulary: persisted payout records (the durable
//! post-burn cash-out log), bounded receipt queries (statements, paginated
//! chains) and account listings. These back the operator dashboard and the
//! statement / payout-tracking HTTP surface without ever loading the whole
//! receipt table into memory on the Postgres path.

use mint_core::{AccountId, Micros};
use serde::{Deserialize, Serialize};

use crate::balance::Balance;

/// Lifecycle of a persisted payout (the off-ramp leg that follows a burn).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayoutRecordStatus {
    /// Accepted by the provider but not yet settled downstream (e.g. a PayPal
    /// payout batch), or not yet attempted.
    Pending,
    /// The provider reports the funds as sent.
    Paid,
    /// The provider rejected / could not send it. The burn is durable; the
    /// payout can be retried against the same `payout_ref`.
    Failed,
    /// Terminal: an operator cancelled a payout that could not be delivered
    /// and re-credited the burned amount to the account.
    Cancelled,
}

impl PayoutRecordStatus {
    /// The stable lowercase wire / column value.
    pub const fn as_str(self) -> &'static str {
        match self {
            PayoutRecordStatus::Pending => "pending",
            PayoutRecordStatus::Paid => "paid",
            PayoutRecordStatus::Failed => "failed",
            PayoutRecordStatus::Cancelled => "cancelled",
        }
    }

    /// Parse the stable column value (`None` for an unknown value).
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(PayoutRecordStatus::Pending),
            "paid" => Some(PayoutRecordStatus::Paid),
            "failed" => Some(PayoutRecordStatus::Failed),
            "cancelled" => Some(PayoutRecordStatus::Cancelled),
            _ => None,
        }
    }
}

/// One durable payout row, keyed by the redeem's `payout_ref`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayoutRecord {
    /// The redeem's `<provider>:<ref>` payout reference (primary key).
    pub payout_ref: String,
    /// The account whose AIT was burned.
    pub account_id: AccountId,
    /// The burned / paid-out amount.
    pub amount_micros: Micros,
    /// The destination in its wire form (`{"kind":"stripe","account":..}` …).
    pub dest: serde_json::Value,
    /// Current lifecycle state.
    pub status: PayoutRecordStatus,
    /// The provider's own id for the payout (Stripe transfer id, PayPal batch
    /// id, tx hash …) once known.
    pub provider_ref: Option<String>,
    /// Number of `initiate_payout` attempts made so far.
    pub attempts: u32,
    /// The last provider error, when the most recent attempt failed.
    pub last_error: Option<String>,
    /// Journal `seq` of the burn this payout settles.
    pub burn_seq: u64,
    /// Creation time (unix ms). Assigned by the store on first insert.
    pub created_ms: i64,
    /// Last update time (unix ms). Assigned by the store on every upsert.
    pub updated_ms: i64,
}

/// Filter for [`crate::LedgerStore::list_payouts`].
#[derive(Clone, Debug, Default)]
pub struct PayoutQuery {
    /// Only payouts in this state.
    pub status: Option<PayoutRecordStatus>,
    /// Only payouts burned from this account.
    pub account_id: Option<AccountId>,
    /// Keyset cursor: only rows created strictly before this `(created_ms,
    /// payout_ref)` pair (newest-first pagination).
    pub before: Option<(i64, String)>,
    /// Maximum rows returned (callers clamp).
    pub limit: usize,
}

/// Bounded per-account receipt query (statements + paginated chain export).
#[derive(Clone, Debug, Default)]
pub struct ReceiptQuery {
    /// Only receipts with `account_seq > after_seq` (cursor).
    pub after_seq: Option<u64>,
    /// Only receipts with `ts >= from_ms`.
    pub from_ms: Option<i64>,
    /// Only receipts with `ts < to_ms`.
    pub to_ms: Option<i64>,
    /// Maximum rows returned (callers clamp).
    pub limit: usize,
}

/// One row of the operator account listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountSummary {
    /// Live projected balance (carries the account id).
    pub balance: Balance,
    /// The opaque external owner reference supplied at creation, if stored.
    pub owner_ref: Option<String>,
    /// Creation time (unix ms), when the backend records it.
    pub created_ms: Option<i64>,
    /// Lifecycle state (active / frozen / closed).
    pub status: crate::operator::AccountStatus,
}

/// Aggregated reserve movements (mints / burns) per provider — the ledger side
/// of reconciliation against provider settlements.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReserveMovementTotal {
    /// `"mint"` (deposit credited) or `"redeem"` (burn for cash-out).
    pub kind: String,
    /// The `<provider>` tag parsed from the deposit/payout ref (`"unknown"`
    /// when the ref carries no tag).
    pub provider: String,
    /// Number of movements.
    pub count: u64,
    /// Absolute total in microdollars.
    pub total_micros: i64,
}

/// The `<provider>` prefix of a namespaced ref (`"stripe:pi_1"` ⇒ `"stripe"`).
pub fn provider_tag_of(r: &str) -> String {
    r.split_once(':')
        .map(|(tag, _)| tag.to_string())
        .filter(|tag| !tag.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}
