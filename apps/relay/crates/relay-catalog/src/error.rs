use thiserror::Error;

pub type Result<T> = std::result::Result<T, CatalogError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CatalogError {
    #[error("catalog source failed: {0}")]
    Source(String),
    #[error("catalog JSON is invalid: {0}")]
    InvalidJson(String),
    #[error("catalog schema version {actual} is unsupported; expected {expected}")]
    UnsupportedSchemaVersion { expected: u16, actual: u16 },
    #[error("catalog revision must not be empty")]
    InvalidRevision,
    #[error("catalog generated_at_ms must not be negative")]
    InvalidGeneratedAt,
    #[error("catalog contains duplicate model or alias '{0}'")]
    DuplicateIdentity(String),
    #[error("catalog model '{0}' has an invalid identifier")]
    InvalidModelId(String),
    #[error("catalog model '{model}' must use provider '{expected}'")]
    InvalidProvider { model: String, expected: String },
    #[error("catalog model '{0}' has an empty display name")]
    InvalidDisplayName(String),
    #[error("catalog model '{model}' has a negative {leg} rate")]
    NegativeRate { model: String, leg: &'static str },
    #[error("catalog model '{0}' has no enabled inference capability")]
    NoCapabilities(String),
    #[error("catalog model was not found: {0}")]
    NotFound(String),
    #[error("catalog is missing enabled prices for configured TensorZero routes: {0}")]
    MissingConfiguredRoutes(String),
    #[error("catalog arithmetic overflow")]
    Overflow,
}
