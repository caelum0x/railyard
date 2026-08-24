//! Hold (escrow) resource handlers.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use mint_core::HoldId;
use mint_ledger::{HoldCaptureRequest, HoldCreateRequest, HoldReleaseRequest};
use mint_store::LedgerStore;

use crate::dto::{parse_account, HoldCaptureReq, HoldCreateReq, HoldReleaseReq, HoldView};
use crate::error::{ok, ok_status, ApiError, ApiResult};
use crate::state::AppState;
use crate::views;

/// `POST /v1/holds` → `Ledger::create_hold`.
pub async fn create<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Json(body): Json<HoldCreateReq>,
) -> ApiResult {
    let from = parse_account(&body.from).map_err(ApiError::from)?;
    let receipt = st
        .ledger
        .create_hold(HoldCreateRequest {
            from,
            amount_micros: body.amount_micros,
            idem_key: body.idem_key,
            ref_: body.ref_,
        })
        .await?;
    Ok(ok_status(
        StatusCode::CREATED,
        views::hold_receipt(&receipt),
    ))
}

/// `POST /v1/holds/:id/capture` → `Ledger::capture_hold`.
pub async fn capture<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
    Json(body): Json<HoldCaptureReq>,
) -> ApiResult {
    let hold_id = HoldId::parse(&id).map_err(ApiError::from)?;
    let to = parse_account(&body.to).map_err(ApiError::from)?;
    let receipt = st
        .ledger
        .capture_hold(HoldCaptureRequest {
            hold_id,
            amount_micros: body.amount_micros,
            to,
            idem_key: body.idem_key,
            ref_: body.ref_,
        })
        .await?;
    Ok(ok(views::hold_receipt(&receipt)))
}

/// `POST /v1/holds/:id/release` → `Ledger::release_hold`.
pub async fn release<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
    Json(body): Json<HoldReleaseReq>,
) -> ApiResult {
    let hold_id = HoldId::parse(&id).map_err(ApiError::from)?;
    let receipt = st
        .ledger
        .release_hold(HoldReleaseRequest {
            hold_id,
            idem_key: body.idem_key,
            ref_: body.ref_,
        })
        .await?;
    Ok(ok(views::hold_receipt(&receipt)))
}

/// `GET /v1/holds/:id` → `store.rebuild_holds` lookup (a rebuild == live gate).
pub async fn get<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
) -> ApiResult {
    let hold_id = HoldId::parse(&id).map_err(ApiError::from)?;
    let holds = st.ledger.store().rebuild_holds().await?;
    let record = holds
        .get(&hold_id)
        .cloned()
        .ok_or(ApiError::Mint(mint_core::MintError::HoldNotFound))?;
    Ok(ok(HoldView::from(record)))
}
