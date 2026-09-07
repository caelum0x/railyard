//! HTTP error mapping. Domain and git-hosting errors are folded into an
//! [`AppError`] that renders a consistent JSON envelope and an appropriate
//! status code — internal details are logged, never leaked to the client.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rithub_core::CoreError;
use rithub_git::GitHostError;
use serde_json::json;

/// A request-handling error with an associated HTTP status.
pub struct AppError {
    status: StatusCode,
    message: String,
}

impl AppError {
    /// A 400 Bad Request with a client-safe message.
    pub fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    /// A 409 Conflict with a client-safe message.
    pub fn conflict(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
        }
    }

    /// A 403 Forbidden with a client-safe message.
    pub fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }

    /// A 404 Not Found with a client-safe message.
    pub fn not_found(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.into(),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        if self.status.is_server_error() {
            tracing::error!(status = %self.status, message = %self.message, "request failed");
        } else {
            tracing::debug!(status = %self.status, message = %self.message, "request rejected");
        }
        (self.status, Json(json!({ "error": self.message }))).into_response()
    }
}

impl From<CoreError> for AppError {
    fn from(error: CoreError) -> Self {
        let status = match &error {
            CoreError::InvalidName { .. } => StatusCode::BAD_REQUEST,
            CoreError::NotFound { .. } => StatusCode::NOT_FOUND,
            CoreError::Conflict { .. } => StatusCode::CONFLICT,
            CoreError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        // Database internals are not client-safe; surface a generic message.
        let message = match &error {
            CoreError::Database(_) => "internal server error".to_string(),
            other => other.to_string(),
        };
        Self { status, message }
    }
}

impl From<GitHostError> for AppError {
    fn from(error: GitHostError) -> Self {
        let status = match &error {
            GitHostError::Invalid(_) => StatusCode::BAD_REQUEST,
            GitHostError::NotFound(_) => StatusCode::NOT_FOUND,
            GitHostError::Rit(_) | GitHostError::Io(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        let message = match &error {
            GitHostError::Invalid(m) => m.clone(),
            GitHostError::NotFound(_) => "not found".to_string(),
            _ => "internal server error".to_string(),
        };
        Self { status, message }
    }
}
