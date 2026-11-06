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

/// Accept either an agent's own derived key (scoped to the `X-Abacus-Agent` it
/// presents, reading its own receipt) or the separate operator key (console /
/// cross-agent lookups).
///
/// ## Real gap closed here (2026-11-06)
///
/// This used to accept the bare `ABACUS_API_KEY` directly, with no agent
/// binding at all — documented at the time (README) as "routes with no agent
/// header ... still take the bare master key, matching their existing
/// single-ingress-secret contract." That was a real, if narrower, instance of
/// the exact bug class `require_api_key` was fixed for: the master secret is
/// supposed to be operator-only and never handed to an agent directly, so in
/// practice this route was reachable only by the operator — but (1) it gave
/// `ABACUS_API_KEY` itself (meant only for *deriving* per-agent keys) the same
/// cross-agent blast radius as the separate, dedicated `ABACUS_ADMIN_API_KEY`,
/// widening what a leaked ingress secret exposes beyond its stated purpose,
/// and (2) it meant no properly-provisioned agent — which per the per-agent
/// model only ever holds ITS OWN derived key, never the master secret — could
/// actually use the endpoint this module's own doc comment says exists for
/// "an agent reading its own spend." Now this route accepts exactly the same
/// per-agent derived key `require_api_key` does (bound to the `X-Abacus-Agent`
/// it names) or the admin key; the bare master secret alone no longer
/// authenticates ANY route directly, closing that gap consistently.
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
    let admin_ok: bool = state
        .admin_api_key
        .as_deref()
        .filter(|k| !k.is_empty())
        .is_some_and(|k| bool::from(k.as_bytes().ct_eq(provided.as_bytes())));
    let agent_header = req
        .headers()
        .get(AGENT_HEADER)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty());
    let ingress_ok: bool = match agent_header {
        Some(agent_id) => {
            !state.ingress_api_key.is_empty()
                && bool::from(
                    derive_agent_key(state.ingress_api_key.as_bytes(), agent_id)
                        .as_bytes()
                        .ct_eq(provided.as_bytes()),
                )
        }
        // No `X-Abacus-Agent` ⇒ there is no agent identity to scope a derived
        // key to; only the separate admin key authenticates an agent-less
        // request on this route (handled by `admin_ok` above). The bare
        // master secret is never accepted directly, on this route or any
        // other.
        None => false,
    };
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
