//! Receipt audit surface — read the system-of-record for one enforced request.
//!
//! Every proxied request that settles writes a [`MintReceipt`](abacus_core::receipt::MintReceipt),
//! Abacus's cryptographically-receipted record of agent spend. The hot path
//! already persists it (via [`crate::settle`]) and the recovery worker projects
//! released receipts, but until now the binary exposed no way to READ one back.
//!
//! `GET /v1/receipts/:request_id` closes that loop: a client authenticates with
//! either its own per-agent derived key (see `auth::require_ingress_or_admin_key`)
//! or the operator key, and looks up the receipt by the stable `x-request-id`
//! correlation it supplied on the proxied call. Mint remains the money
//! authority; this endpoint only returns Abacus's projected receipt view.
//!
//! An agent-scoped caller (one who authenticated with `X-Abacus-Agent` + that
//! exact agent's derived key — the middleware has already cryptographically
//! proven the header is genuine, the same trust the sibling `own_budget`
//! handler places in it) may only read a receipt that actually belongs to
//! that agent; a mismatch reports `404 receipt_not_found`, identical to "never
//! existed", so an agent-scoped caller can't distinguish "exists for a
//! different agent" from "doesn't exist" and can't use this route to probe
//! other agents' request ids. A caller with no agent header has authenticated
//! with the separate admin key (the only other way past the middleware) and
//! may read any receipt, matching `/v1/admin/receipts`.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use abacus_core::envelope::{ErrorBody, ErrorEnvelope, SuccessEnvelope};

use crate::handler::AGENT_HEADER;
use crate::AppState;

/// The maximum accepted `request_id` length, matching the proxy's ingress bound.
const MAX_REQUEST_ID_LEN: usize = 128;

/// `GET /v1/receipts/:request_id` — return the settled receipt for a request id.
///
/// - `200` with `{success:true,data:<receipt>}` when a receipt exists and the
///   caller is either the owning agent or the operator;
/// - `404` with a `receipt_not_found` envelope when none has settled, OR when
///   an agent-scoped caller asks for a receipt that belongs to a different
///   agent (not distinguished from "never existed", see module docs);
/// - `400` when the request id is malformed;
/// - `502` when the receipt store read fails.
pub async fn get_receipt(
    State(state): State<AppState>,
    Path(request_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    if !is_valid_request_id(&request_id) {
        return error(StatusCode::BAD_REQUEST, "bad_id", "request_id is malformed");
    }

    // The middleware only let this request through with EITHER a correctly-
    // derived key for exactly this `X-Abacus-Agent`, or the admin key with no
    // agent header at all (see `require_ingress_or_admin_key`) — so this
    // header, if present, is already cryptographically proven, not a bare
    // caller claim.
    let caller_agent = headers
        .get(AGENT_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty());

    let not_found = || {
        error(
            StatusCode::NOT_FOUND,
            "receipt_not_found",
            "no settled receipt for this request_id",
        )
    };

    match state.receipts.get_by_request_id(&request_id).await {
        Ok(Some(receipt)) => match caller_agent {
            Some(agent_id) if receipt.agent_id.as_str() != agent_id => not_found(),
            _ => (StatusCode::OK, Json(SuccessEnvelope::new(receipt))).into_response(),
        },
        Ok(None) => not_found(),
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
