//! Stored record shapes: API keys, the append-only ledger, and usage rows.
//!
//! These are the persisted data model. The ledger is append-only and immutable:
//! balance is the exact integer sum of every entry's `delta_micros`, never a
//! mutated running total.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::{AccountId, JobId, KeyId, ModelId};
use crate::money::Micros;
use crate::usage::Usage;

/// Unix epoch milliseconds. Simple, serde-friendly, no chrono dependency.
pub type Timestamp = i64;

/// Wall-clock now in unix milliseconds. Monotonicity is not assumed; records
/// are ordered by append order, not by this value.
pub fn now_ms() -> Timestamp {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as Timestamp)
        .unwrap_or(0)
}

/// A stored API key. Only the sha256 hash of the presented bearer is kept — the
/// raw key material is never persisted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredApiKey {
    pub id: KeyId,
    /// sha256 hex of the presented bearer key.
    pub key_hash: String,
    pub account_id: AccountId,
    pub active: bool,
    pub created_at: Timestamp,
}

impl StoredApiKey {
    /// Build an active key row with a fresh id and timestamp.
    pub fn active(key_hash: impl Into<String>, account_id: AccountId) -> Self {
        StoredApiKey {
            id: KeyId::generate(),
            key_hash: key_hash.into(),
            account_id,
            active: true,
            created_at: now_ms(),
        }
    }
}

/// The kind of a ledger entry. Deposit/Release are positive deltas;
/// Reserve/Debit are negative. Balance = exact integer sum of all deltas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LedgerKind {
    /// Funds added to the wallet (positive).
    Deposit,
    /// A hold placed on submit (negative).
    Reserve,
    /// A hold returned — either the full refund on failure or the estimate half
    /// of a settle (positive).
    Release,
    /// The single authoritative charge for a completed job (negative).
    Debit,
}

/// One append-only, immutable ledger entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub id: Uuid,
    pub account_id: AccountId,
    pub kind: LedgerKind,
    /// Signed micro-delta. Deposit/Release positive, Reserve/Debit negative.
    pub delta_micros: Micros,
    /// Free-form label; also tags the `wallet_debit_micros{reason}` metric.
    pub reason: String,
    /// Present for Reserve/Release/Debit; `None` for account-level Deposits.
    pub job_id: Option<JobId>,
    pub created_at: Timestamp,
}

impl LedgerEntry {
    /// Construct an entry with a fresh id and timestamp.
    pub fn new(
        account_id: AccountId,
        kind: LedgerKind,
        delta_micros: Micros,
        reason: impl Into<String>,
        job_id: Option<JobId>,
    ) -> Self {
        LedgerEntry {
            id: Uuid::new_v4(),
            account_id,
            kind,
            delta_micros,
            reason: reason.into(),
            job_id,
            created_at: now_ms(),
        }
    }
}

/// The completed-job counterpart of every Debit. The billing invariant: every
/// Debit has exactly one matching UsageRecord.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRecord {
    pub id: Uuid,
    pub job_id: JobId,
    pub account_id: AccountId,
    pub model: ModelId,
    pub usage: Usage,
    pub created_at: Timestamp,
}

impl UsageRecord {
    /// Construct a usage row with a fresh id and timestamp.
    pub fn new(job_id: JobId, account_id: AccountId, model: ModelId, usage: Usage) -> Self {
        UsageRecord {
            id: Uuid::new_v4(),
            job_id,
            account_id,
            model,
            usage,
            created_at: now_ms(),
        }
    }
}
