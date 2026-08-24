//! Account resource handlers.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use mint_store::LedgerStore;
use serde_json::json;

use crate::dto::{parse_account, AccountView, CreateAccountReq};
use crate::error::{ok, ok_status, ApiError, ApiResult};
use crate::state::AppState;

/// `POST /v1/accounts` → `Ledger::create_account`.
pub async fn create<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Json(body): Json<CreateAccountReq>,
) -> ApiResult {
    let id = st.ledger.create_account(body.kind, body.owner_ref).await?;
    Ok(ok_status(
        StatusCode::CREATED,
        json!({ "account_id": id.as_str() }),
    ))
}

/// `GET /v1/accounts/:id` → existence-checked view (kind is embedded in the id).
pub async fn get<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
) -> ApiResult {
    let account = parse_account(&id).map_err(ApiError::from)?;
    // Confirm the account exists via its balance (AccountNotFound otherwise).
    st.ledger.read_balance(&account).await?;
    let view = AccountView {
        id: account.as_str().to_string(),
        kind: account.kind(),
        status: "active".to_string(),
        created_at: None,
    };
    Ok(ok(view))
}

/// `GET /v1/accounts/:id/balance` → `Ledger::read_balance`.
pub async fn balance<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
) -> ApiResult {
    let account = parse_account(&id).map_err(ApiError::from)?;
    let bal = st.ledger.read_balance(&account).await?;
    Ok(ok(json!({
        "account_id": bal.account_id.as_str(),
        "available_micros": bal.available_micros.get(),
        "held_micros": bal.held_micros.get(),
        "updated_seq": bal.updated_seq,
    })))
}
