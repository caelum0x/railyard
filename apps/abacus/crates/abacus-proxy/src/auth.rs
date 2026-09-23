//! Ingress `X-API-Key` middleware. Every enforced route requires a matching key,
//! compared in CONSTANT TIME (`subtle::ct_eq`) to avoid a timing side channel on
//! the ingress secret. Ported from mint-server `auth.rs`.

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use subtle::ConstantTimeEq;

use abacus_core::envelope::ErrorEnvelope;
use abacus_core::error::AbacusError;

use crate::AppState;

/// Header carrying the ingress API key.
pub const API_KEY_HEADER: &str = "x-api-key";

/// Reject the request with `401` unless `X-API-Key` matches the configured
/// ingress key in constant time.
pub async fn require_api_key(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let provided = req
        .headers()
        .get(API_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected = state.ingress_api_key.as_bytes();
    // ct_eq returns 0 for unequal lengths, then compares in constant time — no
    // early-exit timing leak on the shared prefix.
    let matches: bool = expected.ct_eq(provided.as_bytes()).into();
    if matches && !expected.is_empty() {
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
