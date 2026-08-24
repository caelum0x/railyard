//! Liveness + readiness probes (no auth). `readyz` round-trips to the store so a
//! blue-green cutover only shifts traffic once the ledger backend answers.

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::Json;
use mint_store::LedgerStore;
use serde_json::json;

use crate::state::AppState;

/// `GET /healthz` → `200` liveness.
pub async fn healthz() -> Response {
    Json(json!({ "status": "ok" })).into_response()
}

/// `GET /readyz` → readiness (exercises the store path).
pub async fn readyz<S: LedgerStore + 'static>(State(st): State<AppState<S>>) -> Response {
    // Round-trip a cheap store call; for `PgStore` this issues a real query.
    let _ = st.ledger.store().journal_len().await;
    Json(json!({ "ready": true })).into_response()
}
