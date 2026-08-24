//! Normalized error taxonomy + the billing-engine error, all convertible.
//!
//! `RelayError` is the single vocabulary the HTTP edge maps onto status codes
//! (Auth -> 401, InsufficientCredits -> 402, ProviderKeyMissing -> 503,
//! BadRequest -> 400, Provider/Store -> 502/500). `BillingError` is the
//! ledger-engine failure; it converts to/from `RelayError` at the edge.

use thiserror::Error;

use crate::domain::JobId;
use crate::money::Micros;
use crate::store::StoreError;

/// A provider-layer failure, carrying the upstream message and whether the
/// call is safe to retry.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("provider error: {message} (retryable={retryable})")]
pub struct ProviderError {
    pub message: String,
    pub retryable: bool,
}

impl ProviderError {
    /// A retryable provider failure (network blip / upstream 5xx).
    pub fn retryable(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: true,
        }
    }

    /// A terminal provider failure (client 4xx / bad request upstream).
    pub fn terminal(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            retryable: false,
        }
    }
}

/// The normalized error taxonomy. The variant set is frozen so every layer —
/// and the HTTP edge — reasons about the same closed set of failure modes.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RelayError {
    /// Missing / unknown / inactive API key. Maps to HTTP 401.
    #[error("authentication failed")]
    Auth,

    /// Available balance is below the required estimate. Maps to HTTP 402.
    #[error("insufficient credits")]
    InsufficientCredits,

    /// An upstream provider call failed.
    #[error(transparent)]
    Provider(#[from] ProviderError),

    /// A storage-layer failure (lock poisoned, arithmetic overflow, etc.).
    #[error("store error: {0}")]
    Store(String),

    /// Malformed input, unparseable money string, or money-arithmetic overflow.
    /// Maps to HTTP 400.
    #[error("bad request: {0}")]
    BadRequest(String),

    /// A cataloged provider has no configured API key. Maps to HTTP 503.
    #[error("provider key missing")]
    ProviderKeyMissing,
}

/// Convenience alias for the crate's fallible operations.
pub type Result<T, E = RelayError> = std::result::Result<T, E>;

/// A billing-engine failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BillingError {
    /// The account's available balance cannot cover the requested hold.
    #[error("insufficient credits: available {available:?}, requested {requested:?}")]
    InsufficientCredits {
        available: Micros,
        requested: Micros,
    },

    /// Settle/release was asked to act on a job that was never reserved.
    #[error("unknown reservation for job {0}")]
    UnknownReservation(JobId),

    /// A negative estimate/cost was supplied — money deltas must be well-formed.
    #[error("invalid amount: {0}")]
    InvalidAmount(String),

    /// The underlying store failed (lock poisoned, overflow, backend).
    #[error("store error: {0}")]
    Store(String),
}

impl From<StoreError> for BillingError {
    fn from(e: StoreError) -> Self {
        BillingError::Store(e.to_string())
    }
}

impl From<RelayError> for BillingError {
    /// Money-arithmetic overflow (`checked_*`) surfaces as `RelayError::BadRequest`;
    /// a store failure that was already normalized surfaces as `RelayError::Store`.
    fn from(e: RelayError) -> Self {
        match e {
            RelayError::Store(m) => BillingError::Store(m),
            other => BillingError::InvalidAmount(other.to_string()),
        }
    }
}

impl From<BillingError> for RelayError {
    fn from(e: BillingError) -> Self {
        match e {
            BillingError::InsufficientCredits { .. } => RelayError::InsufficientCredits,
            BillingError::UnknownReservation(_) => RelayError::BadRequest(e.to_string()),
            BillingError::InvalidAmount(m) => RelayError::BadRequest(m),
            BillingError::Store(m) => RelayError::Store(m),
        }
    }
}
