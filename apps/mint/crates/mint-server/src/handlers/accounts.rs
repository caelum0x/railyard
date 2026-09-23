//! Account resource handlers: create, read (real lifecycle row), balance, and
//! the operator lifecycle transitions freeze / unfreeze / close.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use mint_store::{AccountStatus, LedgerStore};
use serde_json::json;

use crate::dto::{parse_account, CreateAccountReq};
use crate::error::{ok, ok_status, ApiError, ApiResult};
use crate::state::AppState;
use crate::views;

/// `POST /v1/accounts` → `Ledger::create_account`.
pub async fn create<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Json(body): Json<CreateAccountReq>,
) -> ApiResult {
    if let Some(owner) = body.owner_ref.as_deref() {
        if owner.len() > 256 {
            return Err(ApiError::BadRequest(
                "owner_ref must be at most 256 characters".into(),
            ));
        }
    }
    let id = st.ledger.create_account(body.kind, body.owner_ref).await?;
    Ok(ok_status(
        StatusCode::CREATED,
        json!({ "account_id": id.as_str() }),
    ))
}

/// `GET /v1/accounts/:id` → the stored account row: kind, owner ref, creation
/// time, lifecycle status and live balance.
pub async fn get<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
) -> ApiResult {
    let account = parse_account(&id).map_err(ApiError::from)?;
    let summary = st.ledger.store().get_account(&account).await?;
    Ok(ok(views::account_summary(&summary)))
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

async fn transition<S: LedgerStore + 'static>(
    st: &AppState<S>,
    id: &str,
    status: AccountStatus,
) -> ApiResult {
    let account = parse_account(id).map_err(ApiError::from)?;
    let summary = st
        .ledger
        .store()
        .set_account_status(&account, status)
        .await?;
    tracing::info!(account = %account.as_str(), status = status.as_str(), "account lifecycle changed");
    Ok(ok(views::account_summary(&summary)))
}

/// `POST /v1/accounts/:id/freeze` → block every outgoing movement (transfers,
/// holds, streams, allowances, AITP settlement, redeem). Incoming credits
/// still land. Operator-only.
pub async fn freeze<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
) -> ApiResult {
    transition(&st, &id, AccountStatus::Frozen).await
}

/// `POST /v1/accounts/:id/unfreeze` → back to `active` (a closed account
/// cannot be reopened: `account_closed`). Operator-only.
pub async fn unfreeze<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
) -> ApiResult {
    transition(&st, &id, AccountStatus::Active).await
}

/// `POST /v1/accounts/:id/close` → terminal close; refused with
/// `account_not_empty` (409) while any available or held balance remains.
/// Operator-only.
pub async fn close<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
) -> ApiResult {
    transition(&st, &id, AccountStatus::Closed).await
}
