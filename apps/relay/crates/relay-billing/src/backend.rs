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
use aitp_client::{HoldSettlementAttestation, PaymentAuthorization, Receipt};

/// Authenticated terminal proof returned by an external billing backend.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalSettlement {
    pub receipt: Option<Receipt>,
    pub attestation: Option<HoldSettlementAttestation>,
}

/// The success-only billing lifecycle: hold an estimate, then settle the exact
/// actual (success) or release the whole hold (failure). Object-safe so the
/// gateway can hold an `Arc<dyn BillingLedger>` and swap backends at wiring time.
#[async_trait]
pub trait BillingLedger: Send + Sync {
    /// Cheap readiness probe for the billing authority. Local ledgers are
    /// always ready; network-backed implementations must override this and
    /// exercise their durable backend without mutating money state.
    async fn health(&self) -> Result<(), BillingError> {
        Ok(())
    }

    /// Resolve the actual backend payer used for a direct Relay principal.
    fn resolve_payer(&self, principal: &AccountId) -> AccountId {
        principal.clone()
    }

    /// Immutable payee to snapshot before reserve. Local ledgers return `None`.
    fn settlement_payee(&self) -> Option<AccountId> {
        None
    }

    /// Confirm that the public AITP address advertised to payers resolves to
    /// the exact backend account used for capture. Local ledgers have no
    /// address registry.
    async fn validate_payee_address(&self, _address: &str) -> Result<(), BillingError> {
        Ok(())
    }

    /// Reserve `estimate` for `job` against `account_id`. Fails with
    /// [`BillingError::InsufficientCredits`] (nothing held) when the account is
    /// short.
    async fn reserve(
        &self,
        account_id: &AccountId,
        estimate: Micros,
        job: JobId,
    ) -> Result<Reservation, BillingError>;

    /// Replay an already-resolved direct reservation command. The durable
    /// coordinator supplies the exact payer and idempotency key committed
    /// before the first backend call, so restart recovery is configuration
    /// independent. Local ledgers can safely use the ordinary reserve path.
    async fn reserve_resolved(
        &self,
        _owner_principal: &AccountId,
        payer_account: &AccountId,
        estimate: Micros,
        job: JobId,
        _idempotency_key: &str,
    ) -> Result<Reservation, BillingError> {
        self.reserve(payer_account, estimate, job).await
    }

    /// Reserve funds from a payer-signed AITP authorization supplied on a 402
    /// retry. Production forwards it to Mint's verifier; local ledgers reserve
    /// against the signed payer account after the edge validates the quote.
    async fn reserve_authorized(
        &self,
        owner_principal: &AccountId,
        authorization: &PaymentAuthorization,
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

    /// Settle and return durable settlement evidence when the backend provides
    /// it. Local ledgers have no external receipt chain and therefore return
    /// `None`; Mint-backed ledgers override this with the signed `AITP-Receipt`.
    async fn settle_with_receipt(
        &self,
        reservation: &Reservation,
        usage: Usage,
        model: ModelId,
    ) -> Result<Option<Receipt>, BillingError> {
        Ok(self
            .settle_with_evidence(reservation, usage, model)
            .await?
            .receipt)
    }

    /// Settle and return the independently verified Mint command attestation.
    async fn settle_with_evidence(
        &self,
        reservation: &Reservation,
        usage: Usage,
        model: ModelId,
    ) -> Result<TerminalSettlement, BillingError> {
        self.settle(reservation, usage, model).await?;
        Ok(TerminalSettlement::default())
    }

    /// Release the whole hold — a failed job costs exactly zero. Idempotent.
    async fn release(&self, reservation: &Reservation) -> Result<(), BillingError>;

    /// Release and return signed terminal evidence when supported.
    async fn release_with_evidence(
        &self,
        reservation: &Reservation,
    ) -> Result<TerminalSettlement, BillingError> {
        self.release(reservation).await?;
        Ok(TerminalSettlement::default())
    }

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

    async fn reserve_authorized(
        &self,
        owner_principal: &AccountId,
        authorization: &PaymentAuthorization,
        estimate: Micros,
        job: JobId,
    ) -> Result<Reservation, BillingError> {
        let mut reservation = Ledger::reserve(
            self,
            &AccountId::new(authorization.payer.clone()),
            estimate,
            job,
        )
        .await?;
        reservation.owner_principal = owner_principal.clone();
        Ok(reservation)
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
