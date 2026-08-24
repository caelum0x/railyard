//! HTTP error envelope. `ApiError` newtypes the domain error so we can implement
//! axum's `IntoResponse` (orphan rule) and map codes to status.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

use passport_core::PassportError;

pub struct ApiError(pub PassportError);

impl From<PassportError> for ApiError {
    fn from(error: PassportError) -> Self {
        Self(error)
    }
}

fn status_for(error: &PassportError) -> StatusCode {
    use PassportError::*;
    match error {
        Invalid(_) | KindMismatch(_) | ResolveUnavailable { .. } | NeedsArtifact { .. } => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        IdempotencyConflict | VersionLocked | VersionNotLocked => StatusCode::CONFLICT,
        NotFound | VersionNotFound => StatusCode::NOT_FOUND,
        Unauthorized => StatusCode::UNAUTHORIZED,
        Relay(_) | Mint(_) => StatusCode::BAD_GATEWAY,
        Storage(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

#[derive(Serialize)]
struct Envelope<'a> {
    error: Body<'a>,
}

#[derive(Serialize)]
struct Body<'a> {
    code: &'a str,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = status_for(&self.0);
        let body = Envelope {
            error: Body {
                code: self.0.code(),
                message: self.0.to_string(),
            },
        };
        (status, Json(body)).into_response()
    }
}
