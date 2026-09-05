//! API error envelope — faithful port of `Api::ApiErrors`.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};

/// A REST error rendered in Lago's envelope shape.
#[derive(Debug)]
pub enum ApiError {
    /// 401 `{ status, error }`.
    Unauthorized,
    /// 400 `{ status, error: "BadRequest: <msg>" }`.
    BadRequest(String),
    /// 403 `{ status, error: "Forbidden", code }`.
    Forbidden(String),
    /// 404 `{ status, error: "Not Found", code: "<resource>_not_found" }`.
    NotFound(String),
    /// 422 `{ status, error, code: "validation_errors", error_details }`.
    Validation(Value),
    /// 500 — internal error (details logged, generic message returned).
    Internal(String),
}

impl ApiError {
    pub fn validation(field: &str, code: &str) -> Self {
        ApiError::Validation(json!({ field: [code] }))
    }
}

impl From<rago_db::DbError> for ApiError {
    fn from(e: rago_db::DbError) -> Self {
        tracing::error!(error = %e, "database error");
        ApiError::Internal("Internal Error".into())
    }
}

impl From<rago_billing::error::BillingError> for ApiError {
    fn from(e: rago_billing::error::BillingError) -> Self {
        ApiError::Validation(json!({ "billing": [e.to_string()] }))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, body) = match self {
            ApiError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                json!({ "status": 401, "error": "Unauthorized" }),
            ),
            ApiError::BadRequest(msg) => (
                StatusCode::BAD_REQUEST,
                json!({ "status": 400, "error": format!("BadRequest: {msg}") }),
            ),
            ApiError::Forbidden(code) => (
                StatusCode::FORBIDDEN,
                json!({ "status": 403, "error": "Forbidden", "code": code }),
            ),
            ApiError::NotFound(resource) => (
                StatusCode::NOT_FOUND,
                json!({ "status": 404, "error": "Not Found", "code": format!("{resource}_not_found") }),
            ),
            ApiError::Validation(details) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({
                    "status": 422,
                    "error": "Unprocessable Entity",
                    "code": "validation_errors",
                    "error_details": details
                }),
            ),
            ApiError::Internal(msg) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({ "status": 500, "error": msg }),
            ),
        };
        (status, Json(body)).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
