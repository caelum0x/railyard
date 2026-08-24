//! Boundary DTOs for the transfer intent and its settlement result.

use mint_core::AccountId;
use mint_store::Balance;

/// A raw transfer intent as received at the API boundary. `amount_micros` is
/// validated (`> 0`) inside [`crate::Ledger::transfer`].
#[derive(Clone, Debug)]
pub struct TransferRequest {
    /// Paying account.
    pub from: AccountId,
    /// Receiving account.
    pub to: AccountId,
    /// Amount in microdollars (validated to be strictly positive).
    pub amount_micros: i64,
    /// Idempotency key: identical replays are no-ops.
    pub idem_key: String,
    /// Optional free-form reference.
    pub ref_: Option<String>,
}

/// The AP0 settlement result (hash-linked signed receipts arrive in AP2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransferReceipt {
    /// `seq` of the debit row (first of the two appended journal rows).
    pub first_seq: u64,
    /// Post-transfer balance of the sender.
    pub from_balance: Balance,
    /// Post-transfer balance of the receiver.
    pub to_balance: Balance,
    /// `true` when this was an idempotent replay (no new rows appended).
    pub idempotent_replay: bool,
}
