//! The billing LIFECYCLE seam the gateway middleware depends on.
//!
//! The middleware only ever drives four operations across the reserve ->
//! settle/release window: [`reserve`](BillingLedger::reserve),
//! [`settle`](BillingLedger::settle), [`release`](BillingLedger::release), and
//! [`available_micros`](BillingLedger::available_micros). Abstracting exactly
//! that surface (rather than the lower-level [`Store`](crate::store::Store)) lets
//! the gateway swap the in-memory [`Ledger`] for the Mint-backed
//! [`MintLedger`](crate::mint::MintLedger) with a single `Arc<dyn BillingLedger>`
//! type change and NO middleware edits.
//!
//! The existing [`Ledger`] (over an `Arc<dyn Store>`, e.g. `InMemoryStore`) keeps
//! a trivial impl so the current unit tests and the dev/test path are unchanged;
//! [`MintLedger`](crate::mint::MintLedger) implements the same trait over HTTP.

use async_trait::async_trait;

use crate::domain::{AccountId, JobId, ModelId};
use crate::error::BillingError;
use crate::ledger::{Ledger, Reservation};
use crate::money::Micros;
use crate::usage::Usage;

/// The success-only billing lifecycle: hold an estimate, then settle the exact
/// actual (success) or release the whole hold (failure). Object-safe so the
/// gateway can hold an `Arc<dyn BillingLedger>` and swap backends at wiring time.
#[async_trait]
pub trait BillingLedger: Send + Sync {
    /// Reserve `estimate` for `job` against `account_id`. Fails with
    /// [`BillingError::InsufficientCredits`] (nothing held) when the account is
    /// short.
    async fn reserve(
        &self,
        account_id: &AccountId,
        estimate: Micros,
        job: JobId,
    ) -> Result<Reservation, BillingError>;

    /// Settle the EXACT actual `usage` cost for a held job (the only debit path).
    /// Idempotent: a repeat on an already-terminal job is a no-op.
    async fn settle(
        &self,
        reservation: &Reservation,
        usage: Usage,
        model: ModelId,
    ) -> Result<(), BillingError>;

    /// Release the whole hold — a failed job costs exactly zero. Idempotent.
    async fn release(&self, reservation: &Reservation) -> Result<(), BillingError>;

    /// The account's available microdollars.
    async fn available_micros(&self, account_id: &AccountId) -> Result<Micros, BillingError>;
}

/// The in-memory / `Store`-backed engine satisfies the seam by delegating to its
/// inherent methods. Kept so existing tests and the dev path work unchanged.
#[async_trait]
impl BillingLedger for Ledger {
    async fn reserve(
        &self,
        account_id: &AccountId,
        estimate: Micros,
        job: JobId,
    ) -> Result<Reservation, BillingError> {
        Ledger::reserve(self, account_id, estimate, job).await
    }

    async fn settle(
        &self,
        reservation: &Reservation,
        usage: Usage,
        model: ModelId,
    ) -> Result<(), BillingError> {
        Ledger::settle(self, reservation, usage, model).await
    }

    async fn release(&self, reservation: &Reservation) -> Result<(), BillingError> {
        Ledger::release(self, reservation).await
    }

    async fn available_micros(&self, account_id: &AccountId) -> Result<Micros, BillingError> {
        Ledger::available_micros(self, account_id).await
    }
}
