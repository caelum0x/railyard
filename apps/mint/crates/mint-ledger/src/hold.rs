//! Boundary DTOs for the hold/escrow intents and their receipts (AP1).
//!
//! A hold reserves value from an account (available→held), then either
//! `capture`s up to the held amount to a payee and releases the remainder, or
//! `release`s the whole reservation with NO settlement (the success-only
//! failure path, ADR-0002). Amounts are integer microdollars, validated at the
//! [`crate::Ledger`] boundary.

use mint_core::{AccountId, HoldId};
use mint_store::{Balance, HoldStatus};

/// A `hold.create` intent as received at the API boundary. `amount_micros` is
/// validated (`> 0`) inside [`crate::Ledger::create_hold`].
#[derive(Clone, Debug)]
pub struct HoldCreateRequest {
    /// Account whose available balance is reserved.
    pub from: AccountId,
    /// Reservation amount in microdollars (validated strictly positive).
    pub amount_micros: i64,
    /// Idempotency key: identical replays are no-ops (same original hold).
    pub idem_key: String,
    /// Optional free-form reference.
    pub ref_: Option<String>,
}

/// A `hold.capture` intent: settle up to the held amount to `to`, release the
/// remainder to the holder.
#[derive(Clone, Debug)]
pub struct HoldCaptureRequest {
    /// The hold to settle.
    pub hold_id: HoldId,
    /// Capture amount in microdollars (validated `> 0` and `<= held`).
    pub amount_micros: i64,
    /// The payee credited with the captured amount.
    pub to: AccountId,
    /// Idempotency key.
    pub idem_key: String,
    /// Optional free-form reference.
    pub ref_: Option<String>,
}

/// A `hold.release` intent: free the whole reservation with no settlement.
#[derive(Clone, Debug)]
pub struct HoldReleaseRequest {
    /// The hold to free.
    pub hold_id: HoldId,
    /// Idempotency key.
    pub idem_key: String,
    /// Optional free-form reference.
    pub ref_: Option<String>,
}

/// The settlement result of a hold op (create/capture/release).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoldReceipt {
    /// The hold id (minted on create; echoed on capture/release).
    pub hold_id: HoldId,
    /// `seq` of the first appended journal row for this op.
    pub first_seq: u64,
    /// Post-op balance of the holder (the reserving account).
    pub holder_balance: Balance,
    /// Post-op balance of the payee (capture only; `None` for create/release).
    pub payee_balance: Option<Balance>,
    /// The hold's lifecycle state after the op.
    pub status: HoldStatus,
    /// The hold's captured amount after the op.
    pub captured_micros: mint_core::Micros,
    /// `true` when this was an idempotent replay (no new rows appended).
    pub idempotent_replay: bool,
}
