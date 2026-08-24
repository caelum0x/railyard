//! Proof-of-reserves handler (§6).

use axum::extract::State;
use mint_store::LedgerStore;
use serde_json::json;

use crate::error::{ok, ApiResult};
use crate::state::AppState;

/// `GET /v1/reserve` → `ReserveLedger::solvency`.
pub async fn get<S: LedgerStore + 'static>(State(st): State<AppState<S>>) -> ApiResult {
    let proof = st.reserve.solvency().await?;
    let cushion = proof
        .backed_micros
        .get()
        .saturating_sub(proof.supply_micros.get());
    Ok(ok(json!({
        "supply_micros": proof.supply_micros.get(),
        "backing_micros": proof.backed_micros.get(),
        "cushion": cushion,
        "ok": proof.ok,
    })))
}
