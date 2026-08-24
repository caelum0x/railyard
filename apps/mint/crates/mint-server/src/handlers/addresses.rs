//! Address resource handlers.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use mint_core::Address;
use mint_store::LedgerStore;
use serde_json::json;

use crate::dto::{parse_account, CreateAddressReq};
use crate::error::{ok_status, ApiError, ApiResult};
use crate::state::AppState;

/// `POST /v1/accounts/:id/addresses` → `Ledger::create_address`.
pub async fn create<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
    Json(body): Json<CreateAddressReq>,
) -> ApiResult {
    let account = parse_account(&id).map_err(ApiError::from)?;
    let addr = st.ledger.create_address(&account, body.purpose).await?;
    Ok(ok_status(
        StatusCode::CREATED,
        json!({ "address": addr.as_str() }),
    ))
}

/// `GET /v1/addresses/:address` → `store.lookup_address`.
pub async fn lookup<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(address): Path<String>,
) -> ApiResult {
    let addr = Address::parse(&address).map_err(ApiError::from)?;
    let account = st.ledger.store().lookup_address(&addr).await?;
    Ok(crate::error::ok(json!({ "account_id": account.as_str() })))
}
