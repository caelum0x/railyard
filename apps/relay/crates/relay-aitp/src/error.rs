use thiserror::Error;

pub type Result<T> = std::result::Result<T, AitpError>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AitpError {
    #[error("AITP amount must be positive and within policy")]
    InvalidAmount,
    #[error("AITP resource is empty or invalid")]
    InvalidResource,
    #[error("AITP challenge expiry overflow")]
    ExpiryOverflow,
    #[error("AITP challenge not found")]
    NotFound,
    #[error("AITP challenge belongs to another authenticated principal")]
    Ownership,
    #[error("AITP authorization does not match its challenge")]
    AuthorizationMismatch,
    #[error("AITP challenge expired")]
    Expired,
    #[error("AITP authorization replay conflicts with an earlier authorization")]
    ConflictingReplay,
    #[error("AITP challenge has already been consumed")]
    AlreadyConsumed,
    #[error("AITP challenge store failed: {0}")]
    Store(String),
    #[error("AITP retention cleanup limit must be positive")]
    InvalidRetentionLimit,
    #[error("AITP wire conversion failed: {0}")]
    Wire(String),
    #[error("settlement evidence is missing Mint's signed attestation")]
    MissingAttestation,
    #[error("Mint settlement attestation is invalid: {0}")]
    InvalidAttestation(String),
    #[error("Mint settlement attestation conflicts with Relay evidence: {0}")]
    EvidenceMismatch(&'static str),
}
