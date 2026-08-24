//! The HTTP error surface: [`ApiError`] maps a domain [`MintError`] (or a
//! boundary validation / auth failure) onto an HTTP status + a stable JSON
//! envelope `{ success:false, error:{ code, message } }` per AITP-402 §7.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use mint_core::MintError;
use serde::Serialize;
use serde_json::json;

/// Every failure the HTTP layer can surface.
#[derive(Debug)]
pub enum ApiError {
    /// A domain error from the ledger/protocol/stream/reserve.
    Mint(MintError),
    /// A boundary validation failure (bad JSON field, unparsable id, etc.).
    BadRequest(String),
    /// Missing / wrong API key.
    Unauthorized,
    /// An infrastructure fault (DB down, signature-verify config missing).
    Internal(String),
}

impl From<MintError> for ApiError {
    fn from(e: MintError) -> Self {
        ApiError::Mint(e)
    }
}

/// Map a stable [`MintError`] code onto its HTTP status (AITP-402 §7 table).
pub fn status_for(err: &MintError) -> StatusCode {
    match err {
        // 402 Payment Required — the caller lacks funds/budget/backing.
        MintError::InsufficientFunds
        | MintError::AllowanceExceeded
        | MintError::ReserveInsufficient
        | MintError::DepositNotConfirmed
        | MintError::DepositAmountMismatch => StatusCode::PAYMENT_REQUIRED,
        // 403 Forbidden — frozen account / insufficient KYC.
        MintError::AccountFrozen | MintError::KycRequired => StatusCode::FORBIDDEN,
        // 404 Not Found — unknown entity.
        MintError::AccountNotFound
        | MintError::HoldNotFound
        | MintError::AllowanceNotFound
        | MintError::StreamNotFound => StatusCode::NOT_FOUND,
        // 409 Conflict — idempotency / nonce replay.
        MintError::IdempotencyConflict | MintError::NonceReplayed => StatusCode::CONFLICT,
        // 422 Unprocessable — semantically invalid but well-formed request.
        MintError::CaptureExceedsHold
        | MintError::AuthorizationExpired
        | MintError::SignatureInvalid
        | MintError::StreamClosed
        | MintError::StreamSeqRegressed
        | MintError::StreamCapExceeded => StatusCode::UNPROCESSABLE_ENTITY,
        // 429 Too Many Requests — payout / velocity limit.
        MintError::PayoutLimitExceeded => StatusCode::TOO_MANY_REQUESTS,
        // 400 Bad Request — malformed identity/amount/address.
        MintError::BadAddress | MintError::BadAccountId | MintError::InvalidAmount => {
            StatusCode::BAD_REQUEST
        }
        // 500 Internal — arithmetic overflow / rebuild mismatch / infra fault.
        MintError::Overflow | MintError::ReceiptRebuildMismatch => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            ApiError::Mint(e) => (status_for(&e), e.code().to_string(), e.to_string()),
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request".to_string(), m),
            ApiError::Unauthorized => (
                StatusCode::UNAUTHORIZED,
                "unauthorized".to_string(),
                "missing or invalid API key".to_string(),
            ),
            ApiError::Internal(m) => (StatusCode::INTERNAL_SERVER_ERROR, "internal".to_string(), m),
        };
        let body = json!({
            "success": false,
            "error": { "code": code, "message": message },
        });
        (status, Json(body)).into_response()
    }
}

/// A success envelope `{ success:true, data }` with `200 OK`.
pub fn ok<T: Serialize>(data: T) -> Response {
    Json(json!({ "success": true, "data": data })).into_response()
}

/// A success envelope with an explicit status (e.g. `201 Created`).
pub fn ok_status<T: Serialize>(status: StatusCode, data: T) -> Response {
    (status, Json(json!({ "success": true, "data": data }))).into_response()
}

/// Shorthand result type for handlers.
pub type ApiResult = Result<Response, ApiError>;
