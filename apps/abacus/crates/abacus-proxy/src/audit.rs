//! Receipt audit surface — read the system-of-record for one enforced request.
//!
//! Every proxied request that settles writes a [`MintReceipt`](abacus_core::receipt::MintReceipt),
//! Abacus's cryptographically-receipted record of agent spend. The hot path
//! already persists it (via [`crate::settle`]) and the recovery worker projects
//! released receipts, but until now the binary exposed no way to READ one back.
//!
//! `GET /v1/receipts/:request_id` closes that loop: a client authenticates with
//! the same ingress key and looks up the receipt by the stable `x-request-id`
//! correlation it supplied on the proxied call. Mint remains the money
//! authority; this endpoint only returns Abacus's projected receipt view.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use abacus_core::envelope::{ErrorBody, ErrorEnvelope, SuccessEnvelope};

use crate::AppState;

/// The maximum accepted `request_id` length, matching the proxy's ingress bound.
const MAX_REQUEST_ID_LEN: usize = 128;

/// `GET /v1/receipts/:request_id` — return the settled receipt for a request id.
///
/// - `200` with `{success:true,data:<receipt>}` when a receipt exists;
/// - `404` with a `receipt_not_found` envelope when none has settled;
/// - `400` when the request id is malformed;
/// - `502` when the receipt store read fails.
pub async fn get_receipt(
    State(state): State<AppState>,
    Path(request_id): Path<String>,
) -> Response {
    if !is_valid_request_id(&request_id) {
        return error(StatusCode::BAD_REQUEST, "bad_id", "request_id is malformed");
    }

    match state.receipts.get_by_request_id(&request_id).await {
        Ok(Some(receipt)) => (StatusCode::OK, Json(SuccessEnvelope::new(receipt))).into_response(),
        Ok(None) => error(
            StatusCode::NOT_FOUND,
            "receipt_not_found",
            "no settled receipt for this request_id",
        ),
        Err(store_error) => {
            tracing::error!(%store_error, %request_id, "receipt lookup failed");
            error(
                StatusCode::BAD_GATEWAY,
                "storage_error",
                "receipt store unavailable",
            )
        }
    }
}

/// Same character/length bound the proxy enforces on `x-request-id`.
fn is_valid_request_id(request_id: &str) -> bool {
    !request_id.is_empty()
        && request_id.len() <= MAX_REQUEST_ID_LEN
        && request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn error(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(ErrorEnvelope {
            success: false,
            error: ErrorBody {
                code: code.to_string(),
                message: message.to_string(),
            },
        }),
    )
        .into_response()
}
