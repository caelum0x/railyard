//! `mint-reserve` — reserve-tracked mint / redeem (AITP-402 §6/§7).
//!
//! Generalizes AP0's single-sided `seed_account` (reserve kept off-book) into a
//! reserve-**TRACKED** mint/redeem so that `Σavailable + Σheld == reserve` holds
//! *by construction*:
//!
//! - **mint** credits `to.available += amount` **and** `reserve_micros += amount`
//!   in one critical section (both rise, LHS == reserve preserved). Gated on a
//!   confirmed injected [`Ramp`] deposit and idempotent on `deposit_ref`.
//! - **redeem** debits `from.available -= amount` **and** `reserve_micros -= amount`
//!   atomically (burn), then initiates a post-commit fiat/crypto payout so a
//!   failed payout can be retried against the already-burned supply without
//!   double-burning. Gated on a [`KycGate`] authorization.
//!
//! [`ReserveLedger::solvency`] reads the in-store `reserve_micros` as `supply`,
//! so `Σavailable + Σheld == reserve` is directly assertable rather than
//! externally supplied.

mod ramp;
mod reserve_ledger;

pub use ramp::{
    Asset, Deposit, FakeKycGate, FakeRamp, KycGate, KycTier, Payout, PayoutDest, PayoutStatus,
    Ramp, RampError, StrictKycGate,
};
pub use reserve_ledger::ReserveLedger;

// Re-export the store/core vocabulary so downstream callers can build a reserve
// ledger and read balances without depending on `mint-store` directly.
pub use mint_core::{AccountId, Micros, MintError};
pub use mint_receipts::ReserveProof;
pub use mint_store::Balance;

/// Boundary request for `mint` (deposit-gated AIT creation).
#[derive(Debug, Clone)]
pub struct MintRequest {
    /// The account credited with freshly-minted AIT.
    pub to: AccountId,
    /// The requested mint amount in microdollars (validated `> 0`).
    pub amount_micros: i64,
    /// The confirmed ramp deposit this mint is gated on (idempotency key).
    pub deposit_ref: String,
    /// A caller idempotency key (folded into the op body; `deposit_ref` is the
    /// authoritative dedupe key).
    pub idem_key: String,
}

/// Boundary request for `redeem` (KYC/limit-gated AIT burn + payout).
#[derive(Debug, Clone)]
pub struct RedeemRequest {
    /// The account whose AIT is burned.
    pub from: AccountId,
    /// The redeem amount in microdollars (validated `> 0`).
    pub amount_micros: i64,
    /// The payout reference (idempotency key for both burn and payout).
    pub payout_ref: String,
    /// The presented KYC tier (authorized against the gate).
    pub kyc_tier: KycTier,
    /// Where the fiat/crypto payout is sent.
    pub payout_dest: PayoutDest,
    /// A caller idempotency key (folded into the op body).
    pub idem_key: String,
}

/// The result of a `mint`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintReceipt {
    /// `seq` of the appended mint row.
    pub first_seq: u64,
    /// Post-mint balance of the credited account.
    pub to_balance: Balance,
    /// The in-store reserve after this mint.
    pub reserve_after_micros: Micros,
    /// The deposit this mint was gated on.
    pub deposit_ref: String,
    /// `true` on an idempotent replay (no second credit).
    pub idempotent_replay: bool,
}

/// The result of a `redeem`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeemReceipt {
    /// `seq` of the appended redeem (burn) row.
    pub first_seq: u64,
    /// Post-burn balance of the debited account.
    pub from_balance: Balance,
    /// The in-store reserve after this redeem.
    pub reserve_after_micros: Micros,
    /// The payout reference.
    pub payout_ref: String,
    /// The initiated payout (status `Failed` on a transient provider outage —
    /// retry `redeem` with the same `payout_ref`).
    pub payout: Payout,
    /// `true` on an idempotent replay (no second burn).
    pub idempotent_replay: bool,
}
