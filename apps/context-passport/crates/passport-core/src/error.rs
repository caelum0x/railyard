//! Stable, transport-agnostic domain errors. HTTP mapping lives in the server.

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PassportError {
    #[error("invalid passport: {0}")]
    Invalid(String),
    #[error("idempotency key was reused with different creation data")]
    IdempotencyConflict,
    #[error("passport not found")]
    NotFound,
    #[error("passport version not found")]
    VersionNotFound,
    #[error("this version is locked and immutable; branch a new version to edit")]
    VersionLocked,
    #[error("this operation requires a locked version, but the head is still a draft")]
    VersionNotLocked,
    #[error("passport kind mismatch: {0}")]
    KindMismatch(String),
    #[error("no conditioning available for provider '{provider}' modality '{modality}'")]
    ResolveUnavailable { provider: String, modality: String },
    #[error("provider '{provider}' needs a trained artifact for modality '{modality}' that this passport does not carry")]
    NeedsArtifact { provider: String, modality: String },
    #[error("relay generation failed: {0}")]
    Relay(String),
    #[error("mint metering failed: {0}")]
    Mint(String),
    #[error("storage failed: {0}")]
    Storage(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("passport is archived; unarchive it to edit, lock, branch or generate")]
    Archived,
    #[error("trained artifact not found")]
    ArtifactNotFound,
    #[error("artifact transition not allowed: {0}")]
    ArtifactState(String),
}

impl PassportError {
    /// Stable machine code for the HTTP error envelope.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid_passport",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::NotFound => "not_found",
            Self::VersionNotFound => "version_not_found",
            Self::VersionLocked => "version_locked",
            Self::VersionNotLocked => "version_not_locked",
            Self::KindMismatch(_) => "kind_mismatch",
            Self::ResolveUnavailable { .. } => "resolve_unavailable",
            Self::NeedsArtifact { .. } => "needs_artifact",
            Self::Relay(_) => "relay_unavailable",
            Self::Mint(_) => "mint_unavailable",
            Self::Storage(_) => "storage_unavailable",
            Self::Unauthorized => "unauthorized",
            Self::Archived => "passport_archived",
            Self::ArtifactNotFound => "artifact_not_found",
            Self::ArtifactState(_) => "artifact_state_conflict",
        }
    }
}
