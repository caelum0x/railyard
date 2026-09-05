use thiserror::Error;

/// Errors surfaced by the billing services. Mirrors Lago's `BaseService`
/// `Result` failure modes (`service_failure!`, `NotImplementedError`, …).
#[derive(Debug, Error)]
pub enum BillingError {
    #[error("charge model `{0}` is not implemented")]
    ChargeModelNotImplemented(String),

    #[error("aggregation type `{0}` is not implemented")]
    AggregationNotImplemented(String),

    #[error("invalid charge properties: {0}")]
    InvalidProperties(String),

    #[error("currency mismatch: {0}")]
    CurrencyMismatch(String),

    #[error("aggregation failure: {0}")]
    AggregationFailure(String),
}

pub type Result<T> = std::result::Result<T, BillingError>;
