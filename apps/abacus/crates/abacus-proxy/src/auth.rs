//! Ingress `X-API-Key` middleware. Every enforced route requires a matching key,
//! compared in CONSTANT TIME (`subtle::ct_eq`) to avoid a timing side channel on
//! the ingress secret. Ported from mint-server `auth.rs`.
//!
//! ## Per-agent key binding (closes cross-agent impersonation)
//!
//! `ABACUS_API_KEY` is the ONE secret the operator holds; it is never handed to
//! agents directly. Instead, each configured agent is issued its own derived key
//! — [`derive_agent_key`] — `hex(HMAC-SHA256(ABACUS_API_KEY, agent_id))`. When a
//! request carries `X-Abacus-Agent`, [`require_api_key`] recomputes that exact
//! agent's derived key and compares it, in constant time, to the presented
//! `X-API-Key`. An agent that only knows its own derived key cannot compute (or
//! guess) another agent's — HMAC preimage resistance — so setting
//! `X-Abacus-Agent` to someone else's id while presenting your own key always
//! fails here, before [`crate::local_store::LocalStore::resolve`] or
//! [`crate::postgres_product_store::PostgresProductStore`]'s resolve ever runs a
//! bare lookup on the caller-supplied id. This is what cryptographically binds
//! the header to the key; a plain string-equality lookup never could, because
//! every agent would still be comparing against the SAME shared secret.

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use abacus_core::envelope::ErrorEnvelope;
use abacus_core::error::AbacusError;

use crate::handler::AGENT_HEADER;
use crate::AppState;

/// Header carrying the ingress API key.
pub const API_KEY_HEADER: &str = "x-api-key";

type HmacSha256 = Hmac<Sha256>;

/// Derive the per-agent ingress key an operator issues to ONE configured agent.
///
/// `master_secret` is the operator-only `ABACUS_API_KEY`; `agent_id` is the
/// EXACT `agent_…` string from that agent's entry in `ABACUS_AGENT_CONFIG`. The
/// operator runs this once per agent (offline, e.g. via a short script or
/// `openssl dgst -sha256 -hmac "$ABACUS_API_KEY"`) and hands the agent ONLY the
/// hex output below — never the master secret itself.
pub fn derive_agent_key(master_secret: &[u8], agent_id: &str) -> String {
    // HMAC accepts a key of any length, so this never panics on a validated,
    // non-empty `ABACUS_API_KEY` (enforced by `Config::from_env`'s `require`).
    let mut mac =
        HmacSha256::new_from_slice(master_secret).expect("HMAC-SHA256 accepts any key length");
    mac.update(agent_id.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Reject the request with `401` unless `X-API-Key` matches the expected key in
/// constant time. When the request names an agent (`X-Abacus-Agent`), the
/// expected key is THAT agent's derived key, not the raw master secret — see the
/// module docs. Without an agent header, the bare master secret is required, so
/// a request missing `X-Abacus-Agent` still reaches the handler's existing
/// `400 bad_id` rather than being masked by a `401` here.
pub async fn require_api_key(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let provided = req
        .headers()
        .get(API_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let agent_header = req
        .headers()
        .get(AGENT_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty());
    let expected = match agent_header {
        Some(agent_id) => derive_agent_key(state.ingress_api_key.as_bytes(), agent_id),
        None => state.ingress_api_key.clone(),
    };
    // ct_eq returns 0 for unequal lengths, then compares in constant time — no
    // early-exit timing leak on the shared prefix.
    let matches: bool = expected.as_bytes().ct_eq(provided.as_bytes()).into();
    if matches && !state.ingress_api_key.is_empty() {
        next.run(req).await
    } else {
        let err = AbacusError::Unauthorized;
        (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(ErrorEnvelope::from_error(&err)),
        )
            .into_response()
    }
}

/// Reject the request unless `X-API-Key` matches the SEPARATE operator key
/// (`ABACUS_ADMIN_API_KEY`), in constant time. The agents' ingress key never
/// grants operator views. With no operator key configured the operator API is
/// disabled (`503`), never silently opened.
pub async fn require_admin_key(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let Some(expected) = state.admin_api_key.as_deref().filter(|k| !k.is_empty()) else {
        return (
            axum::http::StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "success": false,
                "error": {
                    "code": "admin_disabled",
                    "message": "operator API disabled: set ABACUS_ADMIN_API_KEY"
                }
            })),
        )
            .into_response();
    };
    let provided = req
        .headers()
        .get(API_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let matches: bool = expected.as_bytes().ct_eq(provided.as_bytes()).into();
    if matches {
        next.run(req).await
    } else {
        let err = AbacusError::Unauthorized;
        (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(ErrorEnvelope::from_error(&err)),
        )
            .into_response()
    }
}

/// Accept either the ingress key (an agent reading its own receipt) or the
/// operator key (console lookups).
pub async fn require_ingress_or_admin_key(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Response {
    let provided = req
        .headers()
        .get(API_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let ingress = state.ingress_api_key.as_bytes();
    let ingress_ok: bool = !ingress.is_empty() && bool::from(ingress.ct_eq(provided.as_bytes()));
    let admin_ok: bool = state
        .admin_api_key
        .as_deref()
        .filter(|k| !k.is_empty())
        .is_some_and(|k| bool::from(k.as_bytes().ct_eq(provided.as_bytes())));
    if ingress_ok || admin_ok {
        next.run(req).await
    } else {
        let err = AbacusError::Unauthorized;
        (
            axum::http::StatusCode::UNAUTHORIZED,
            Json(ErrorEnvelope::from_error(&err)),
        )
            .into_response()
    }
}
