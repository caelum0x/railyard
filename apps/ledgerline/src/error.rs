//! Stable service errors and HTTP envelopes.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LedgerlineError {
    #[error("invalid event: {0}")]
    Invalid(String),
    #[error("idempotency key or request id was reused with different event data")]
    IdempotencyConflict,
    #[error("margin record not found")]
    NotFound,
    #[error("money arithmetic overflow")]
    Overflow,
    #[error("Mint settlement failed: {0}")]
    Mint(String),
    #[error("storage failed: {0}")]
    Storage(String),
    #[error("unauthorized")]
    Unauthorized,
    #[error("forbidden: {0}")]
    Forbidden(String),
    /// Another worker holds this settlement's lease (or took it over); retry
    /// later. No money moved on this call.
    #[error("settlement is in progress elsewhere; retry later")]
    InProgress,
}

impl LedgerlineError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Invalid(_) => "invalid_event",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::NotFound => "not_found",
            Self::Overflow => "money_overflow",
            Self::Mint(_) => "mint_unavailable",
            Self::Storage(_) => "storage_unavailable",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden(_) => "forbidden",
            Self::InProgress => "settlement_in_progress",
        }
    }

    pub const fn status(&self) -> StatusCode {
        match self {
            Self::Invalid(_) | Self::Overflow => StatusCode::UNPROCESSABLE_ENTITY,
            Self::IdempotencyConflict | Self::InProgress => StatusCode::CONFLICT,
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::Mint(_) => StatusCode::BAD_GATEWAY,
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

impl IntoResponse for LedgerlineError {
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
