//! Liveness + readiness probes (no auth). `readyz` round-trips to the store so a
//! blue-green cutover only shifts traffic once the ledger backend answers.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use mint_store::LedgerStore;
use serde_json::json;

use crate::state::AppState;

/// `GET /healthz` → `200` liveness.
pub async fn healthz() -> Response {
    Json(json!({ "status": "ok" })).into_response()
}

/// `GET /readyz` → readiness. Issues a real store query (the journal count and
/// the reserve singleton); any storage fault reports `503 not ready` so a load
/// balancer never routes money traffic to an instance whose ledger is down.
pub async fn readyz<S: LedgerStore + 'static>(State(st): State<AppState<S>>) -> Response {
    let store = st.ledger.store();
    let journal = store.try_journal_len().await;
    let reserve = store.try_reserve_micros().await;
    match (journal, reserve) {
        (Ok(_), Ok(_)) => Json(json!({ "ready": true })).into_response(),
        (j, r) => {
            let failed = if j.is_err() { "journal" } else { "reserve" };
            let code = j.err().or(r.err()).map(|e| e.code()).unwrap_or("storage");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "ready": false, "failed_check": failed, "error": code })),
            )
                .into_response()
        }
    }
}
