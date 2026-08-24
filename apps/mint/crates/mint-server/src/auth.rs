//! API-key auth middleware. Every route except `/healthz` and `/readyz` requires
//! a matching `X-API-Key` header, compared in constant time to avoid a timing
//! side channel on the secret.

use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use mint_store::LedgerStore;
use subtle::ConstantTimeEq;

use crate::error::ApiError;
use crate::state::AppState;

/// Header carrying the API key.
pub const API_KEY_HEADER: &str = "x-api-key";

/// Reject the request with `401` unless `X-API-Key` matches the configured key
/// in constant time.
pub async fn require_api_key<S: LedgerStore + 'static>(
    State(state): State<AppState<S>>,
    req: Request,
    next: Next,
) -> Result<Response, ApiError> {
    let provided = req
        .headers()
        .get(API_KEY_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected = state.api_key.as_bytes();
    // subtle's slice `ct_eq` returns 0 for unequal lengths, then compares bytes
    // in constant time — no early-exit timing leak on the shared prefix.
    let matches: bool = expected.ct_eq(provided.as_bytes()).into();
    if matches && !expected.is_empty() {
        Ok(next.run(req).await)
    } else {
        Err(ApiError::Unauthorized)
    }
}
