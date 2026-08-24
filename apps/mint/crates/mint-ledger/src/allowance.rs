//! Boundary DTOs for the delegated-allowance verb (AITP-402 §4, AP3).
//!
//! An allowance is a delegated spend budget: a `granter` authorizes a delegate
//! (`to_key`) to spend up to `limit_micros` within a [`AllowanceWindow`],
//! optionally restricted by an [`AllowanceScope`]. Each `spend` auto-settles by
//! either transferring from the granter or opening a hold against it — flowing
//! through the SAME atomic journal/receipt path as a plain transfer/hold, so
//! conservation and receipt-chain integrity hold. Amounts are integer
//! microdollars, validated at the [`crate::Ledger`] boundary.

use mint_core::{AccountId, AllowanceId, Micros};
use mint_store::{AllowanceScheme, AllowanceScope, AllowanceWindow};

/// An `allowance.grant` intent. `limit_micros` is validated (`> 0`) inside
/// [`crate::Ledger::grant_allowance`].
#[derive(Clone, Debug)]
pub struct AllowanceGrantRequest {
    /// Account delegating the budget (must exist).
    pub granter: AccountId,
    /// Opaque key identifying the delegate the budget is granted to.
    pub to_key: String,
    /// Spend cap in microdollars (validated strictly positive).
    pub limit_micros: i64,
    /// Reset policy (per-call cumulative vs rolling window).
    pub window: AllowanceWindow,
    /// Optional payee/resource restriction (`None` = unrestricted).
    pub scope: Option<AllowanceScope>,
    /// Idempotency key: identical replays return the same allowance id.
    pub idem_key: String,
}

/// An `allowance.spend` intent: draw `amount_micros` from an allowance,
/// auto-settling to `to` via the chosen [`AllowanceScheme`].
#[derive(Clone, Debug)]
pub struct AllowanceSpendRequest {
    /// The allowance to draw against.
    pub allowance_id: AllowanceId,
    /// Spend amount in microdollars (validated `> 0`, then budget-checked).
    pub amount_micros: i64,
    /// The payee credited (transfer) or eventual capture target (hold).
    pub to: AccountId,
    /// Auto-settle via transfer (debit granter / credit `to`) or hold (reserve
    /// against the granter, later captured/released).
    pub scheme: AllowanceScheme,
    /// Optional free-form reference (matched against `scope.resource_prefix`).
    pub ref_: Option<String>,
    /// Idempotency key: identical replays move money once.
    pub idem_key: String,
}

/// The settlement result of an `allowance.spend`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpendReceipt {
    /// The allowance drawn against.
    pub allowance_id: AllowanceId,
    /// Remaining budget in the current window after this spend.
    pub remaining_micros: Micros,
    /// `seq` of the first journal row appended by the auto-settlement.
    pub settlement_seq: u64,
    /// The hold minted against the granter for a `Hold` scheme (`None` for
    /// `Transfer`).
    pub hold_id: Option<mint_core::HoldId>,
    /// `true` when this was an idempotent replay (no money moved).
    pub idempotent_replay: bool,
}
