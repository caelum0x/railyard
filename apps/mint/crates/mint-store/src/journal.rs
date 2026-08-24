//! Append-only journal row shape (the source of truth).
//!
//! A transfer appends exactly two rows sharing `op`/`op_hash`/`idem_key`: a
//! debit (`-amount`) on the sender and a credit (`+amount`) on the receiver, so
//! the two rows sum to zero. An AP0 opening-balance seed appends a single row
//! with `counter_id = None` (the reserve counterparty is off-book until AP4).

use mint_core::{AccountId, Micros, OpHash};
use serde::{Deserialize, Serialize};

/// The ledger verb. AP0 has `Transfer` + the `Seed` opening entry; AP1 adds the
/// three hold verbs. A hold op's rows move value between an account's own
/// `Available`/`Held` buckets (and, on capture, settle to a payee), so the
/// signed rows of every hold op net to zero across buckets.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum OpKind {
    /// Atomic double-entry debit→credit between two accounts.
    Transfer,
    /// AP0-only single-sided opening balance (reserve side off-book).
    Seed,
    /// AP1: reserve `amount` from an account (available→held).
    HoldCreate,
    /// AP1: settle up to the held amount to a payee and release the remainder.
    HoldCapture,
    /// AP1: free the whole hold with no settlement (success-only failure path).
    HoldRelease,
    /// AP4: open a metered stream — reserve the whole cap (available→held) on
    /// the payer. Conservation-neutral (never touches the reserve).
    StreamOpen,
    /// AP4: settle one stream tick — move `units*rate` held(payer)→available(payee).
    StreamTick,
    /// AP4: close a stream — optional final tick then release the unused
    /// remainder held(payer)→available(payer).
    StreamClose,
    /// AP4: create AIT against a confirmed deposit. A single-sided account credit
    /// (`counter_id = None`, `ref_ = deposit_ref`); the store's `reserve_micros`
    /// rises by the same amount in the SAME critical section.
    Mint,
    /// AP4: burn AIT against a payout. A single-sided account debit
    /// (`counter_id = None`, `ref_ = payout_ref`); `reserve_micros` falls by the
    /// same amount in the SAME critical section.
    Redeem,
}

/// Which balance leg a journal row moves. AP0 rows are all `Available`; AP1 hold
/// rows route their signed delta to the correct leg so a within-account
/// reservation is expressible in the append-only journal.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub enum Bucket {
    /// The spendable leg (`available_micros`). Default for byte-compat with
    /// pre-AP1 rows that omit the field.
    #[default]
    Available,
    /// The reserved leg (`held_micros`).
    Held,
}

/// One append-only journal entry. Never updated or deleted.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Global monotonic sequence number (0-based).
    pub seq: u64,
    /// The operation verb.
    pub op: OpKind,
    /// blake3 hash of the canonical operation body (shared across an op's rows).
    pub op_hash: OpHash,
    /// Idempotency key (shared across an op's rows).
    pub idem_key: String,
    /// The account this row moves.
    pub account_id: AccountId,
    /// The counterparty account (`None` for single-sided seeds).
    pub counter_id: Option<AccountId>,
    /// Signed microdollar delta. The two rows of a transfer sum to zero.
    pub delta_micros: Micros,
    /// Which balance leg this row moves (`Available` for AP0 rows and, via
    /// `#[serde(default)]`, for any pre-AP1 row that omits the field).
    #[serde(default)]
    pub bucket: Bucket,
    /// Optional free-form reference (`ref` in SQL; `ref` is a Rust keyword). For
    /// hold rows this carries the `hold_id`, linking capture/release to create.
    pub ref_: Option<String>,
    /// Timestamp (unix milliseconds) from the injected clock.
    pub ts: i64,
}
