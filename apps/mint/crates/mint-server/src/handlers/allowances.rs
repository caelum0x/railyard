//! Allowance (delegated budget) resource handlers.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use mint_core::AllowanceId;
use mint_ledger::{AllowanceGrantRequest, AllowanceSpendRequest};
use mint_store::LedgerStore;
use serde_json::json;

use crate::dto::{
    parse_account, AllowanceGrantReq, AllowanceRevokeReq, AllowanceSpendReq, AllowanceView,
};
use crate::error::{ok, ok_status, ApiError, ApiResult};
use crate::state::AppState;
use crate::views;

/// `POST /v1/allowances` → `Ledger::grant_allowance`.
pub async fn grant<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Json(body): Json<AllowanceGrantReq>,
) -> ApiResult {
    let granter = parse_account(&body.granter).map_err(ApiError::from)?;
    let id = st
        .ledger
        .grant_allowance(AllowanceGrantRequest {
            granter,
            to_key: body.to_key,
            limit_micros: body.limit_micros,
            window: body.window,
            scope: body.scope,
            idem_key: body.idem_key,
        })
        .await?;
    Ok(ok_status(
        StatusCode::CREATED,
        json!({ "allowance_id": id.as_str() }),
    ))
}

/// `POST /v1/allowances/:id/spend` → `Ledger::spend_allowance`.
pub async fn spend<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
    Json(body): Json<AllowanceSpendReq>,
) -> ApiResult {
    let allowance_id = AllowanceId::parse(&id).map_err(ApiError::from)?;
    let to = parse_account(&body.to).map_err(ApiError::from)?;
    let receipt = st
        .ledger
        .spend_allowance(AllowanceSpendRequest {
            allowance_id,
            amount_micros: body.amount_micros,
            to,
            scheme: body.scheme,
            ref_: body.ref_,
            idem_key: body.idem_key,
        })
        .await?;
    Ok(ok(views::spend_receipt(&receipt)))
}

/// `POST /v1/allowances/:id/revoke` → `Ledger::revoke_allowance`.
pub async fn revoke<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
    Json(body): Json<AllowanceRevokeReq>,
) -> ApiResult {
    let allowance_id = AllowanceId::parse(&id).map_err(ApiError::from)?;
    st.ledger
        .revoke_allowance(allowance_id, body.idem_key)
        .await?;
    Ok(ok(json!({ "revoked": true })))
}

/// `GET /v1/allowances/:id` → `Ledger::read_allowance`.
pub async fn get<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
) -> ApiResult {
    let allowance_id = AllowanceId::parse(&id).map_err(ApiError::from)?;
    let record = st.ledger.read_allowance(&allowance_id).await?;
    Ok(ok(AllowanceView::from(record)))
}
