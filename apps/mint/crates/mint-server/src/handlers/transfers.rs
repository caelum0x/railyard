//! Transfer resource handler.

use axum::extract::State;
use axum::Json;
use mint_ledger::TransferRequest;
use mint_store::LedgerStore;
use serde_json::json;

use crate::dto::{parse_account, TransferReq};
use crate::error::{ok, ApiError, ApiResult};
use crate::state::AppState;

/// `POST /v1/transfers` → `Ledger::transfer`.
pub async fn create<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Json(body): Json<TransferReq>,
) -> ApiResult {
    let from = parse_account(&body.from).map_err(ApiError::from)?;
    let to = parse_account(&body.to).map_err(ApiError::from)?;
    let receipt = st
        .ledger
        .transfer(TransferRequest {
            from,
            to,
            amount_micros: body.amount_micros,
            idem_key: body.idem_key,
            ref_: body.ref_,
        })
        .await?;
    Ok(ok(json!({
        "first_seq": receipt.first_seq,
        "from_balance": serde_json::to_value(&receipt.from_balance).unwrap_or(json!(null)),
        "to_balance": serde_json::to_value(&receipt.to_balance).unwrap_or(json!(null)),
        "idempotent_replay": receipt.idempotent_replay,
    })))
}
