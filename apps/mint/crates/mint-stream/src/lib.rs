//! `mint-stream` — AP4 streaming settlement (metered holds).
//!
//! A **stream** is an AP1 hold whose cap is settled *incrementally*: opening a
//! stream reserves the whole cap up-front (`-cap Available`, `+cap Held` on the
//! payer), each **tick** moves `units * rate` from Held → payee Available, and
//! **close** performs an optional final tick then releases the unused remainder
//! (Held → payer Available). Because every microAIT that leaves the payer's Held
//! bucket lands in the payee's Available bucket (or returns to the payer on
//! close), a stream is conservation-**NEUTRAL** — it never touches the reserve.
//!
//! [`StreamLedger`] is the store-agnostic boundary: it validates intents
//! (`rate > 0`, `cap > 0`, `from != to`), computes the canonical `op_hash`, and
//! delegates the atomic commit to `store.commit_stream_*`. All locking,
//! journalling, receipts, and projection live in `mint-store`.

mod stream_ledger;

pub use stream_ledger::StreamLedger;

// Re-export the store/core streaming vocabulary so downstream callers can build
// a stream ledger without depending on `mint-store` directly.
pub use mint_core::{AccountId, Micros, MintError, StreamId};
pub use mint_store::{Balance, StreamRecord, StreamStatus, StreamUnit};

/// Boundary request for `open_stream` (payer reserves the whole cap up-front).
#[derive(Debug, Clone)]
pub struct OpenStreamRequest {
    /// The payer whose available balance backs the channel.
    pub from: AccountId,
    /// The payee each tick settles to.
    pub to: AccountId,
    /// Per-unit price in microdollars (validated `> 0`).
    pub rate_micros: i64,
    /// The whole up-front reservation (validated `> 0`).
    pub cap_micros: i64,
    /// Metering granularity (per-second vs per-unit).
    pub unit: StreamUnit,
    /// Idempotency key: identical replays are no-ops (same original stream id).
    pub idem_key: String,
    /// Optional free-form reference.
    pub ref_: Option<String>,
}

/// Boundary request for `tick_stream` (settle `units * rate` Held → payee).
#[derive(Debug, Clone)]
pub struct TickStreamRequest {
    /// The stream id string (`strm_<ULID>`), parsed at the boundary.
    pub stream_id: String,
    /// Units metered this tick.
    pub units: u64,
    /// Strictly-increasing tick sequence number.
    pub seq: u64,
    /// Idempotency key.
    pub idem_key: String,
    /// Optional free-form reference.
    pub ref_: Option<String>,
}

/// Boundary request for `close_stream` (optional final tick, then release the
/// unused remainder).
#[derive(Debug, Clone)]
pub struct CloseStreamRequest {
    /// The stream id string.
    pub stream_id: String,
    /// Final-tick units (may be `0` to just release the remainder).
    pub units: u64,
    /// The final-tick sequence number (strictly `> last_seq` when `units > 0`).
    pub seq: u64,
    /// Idempotency key.
    pub idem_key: String,
    /// Optional free-form reference.
    pub ref_: Option<String>,
}

/// The result of `open_stream`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOpenReceipt {
    /// The minted (or replayed) stream id.
    pub stream_id: StreamId,
    /// `seq` of the first appended row.
    pub first_seq: u64,
    /// Post-op payer balance (available dropped by cap, held rose by cap).
    pub holder_balance: Balance,
    /// `true` on an idempotent replay.
    pub idempotent_replay: bool,
}

/// The result of `tick_stream`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamTickReceipt {
    /// The stream ticked.
    pub stream_id: StreamId,
    /// The applied tick `seq`.
    pub tick_seq: u64,
    /// Cumulative settled amount after this tick.
    pub settled_total_micros: Micros,
    /// Post-op payer balance.
    pub from_balance: Balance,
    /// Post-op payee balance.
    pub to_balance: Balance,
    /// `true` on an idempotent replay.
    pub idempotent_replay: bool,
}

/// The result of `close_stream`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamCloseReceipt {
    /// The stream closed.
    pub stream_id: StreamId,
    /// Cumulative settled amount (incl. any final tick).
    pub settled_total_micros: Micros,
    /// The unused remainder released back to the payer.
    pub released_micros: Micros,
    /// Post-op payer balance.
    pub from_balance: Balance,
    /// Post-op payee balance.
    pub to_balance: Balance,
    /// `true` on an idempotent replay.
    pub idempotent_replay: bool,
}
