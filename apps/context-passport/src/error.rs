//! Stable service errors and their HTTP envelopes.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

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
    #[error("storage failed: {0}")]
    Storage(String),
    #[error("unauthorized")]
    Unauthorized,
}

impl PassportError {
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
            Self::Storage(_) => "storage_unavailable",
            Self::Unauthorized => "unauthorized",
        }
    }

    pub const fn status(&self) -> StatusCode {
        match self {
            Self::Invalid(_) | Self::KindMismatch(_) => StatusCode::UNPROCESSABLE_ENTITY,
            Self::IdempotencyConflict | Self::VersionLocked | Self::VersionNotLocked => {
                StatusCode::CONFLICT
            }
            Self::NotFound | Self::VersionNotFound => StatusCode::NOT_FOUND,
            Self::ResolveUnavailable { .. } => StatusCode::UNPROCESSABLE_ENTITY,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Storage(_) => StatusCode::SERVICE_UNAVAILABLE,
        }
    }
}

#[derive(Debug, Serialize)]
struct ErrorEnvelope<'a> {
    error: ErrorBody<'a>,
}

#[derive(Debug, Serialize)]
struct ErrorBody<'a> {
    code: &'static str,
    message: &'a str,
}

impl IntoResponse for PassportError {
    fn into_response(self) -> Response {
        let status = self.status();
        let message = self.to_string();
        (
            status,
            Json(ErrorEnvelope {
                error: ErrorBody {
                    code: self.code(),
                    message: &message,
                },
            }),
        )
            .into_response()
    }
}
