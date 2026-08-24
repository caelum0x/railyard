//! Deposit-gated mint / KYC-gated redeem handlers (§6/§7).

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use mint_reserve::{MintRequest, RedeemRequest};
use mint_store::LedgerStore;

use crate::dto::{parse_account, MintReq, RedeemReq};
use crate::error::{ok_status, ApiError, ApiResult};
use crate::state::AppState;
use crate::views;

/// `POST /v1/mint` → `ReserveLedger::mint` (deposit-gated).
pub async fn mint<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Json(body): Json<MintReq>,
) -> ApiResult {
    let to = parse_account(&body.to).map_err(ApiError::from)?;
    let receipt = st
        .reserve
        .mint(MintRequest {
            to,
            amount_micros: body.amount_micros,
            deposit_ref: body.deposit_ref,
            idem_key: body.idem_key,
        })
        .await?;
    Ok(ok_status(
        StatusCode::CREATED,
        views::mint_receipt(&receipt),
    ))
}

/// `POST /v1/redeem` → `ReserveLedger::redeem` (KYC/limit-gated).
pub async fn redeem<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Json(body): Json<RedeemReq>,
) -> ApiResult {
    let from = parse_account(&body.from).map_err(ApiError::from)?;
    let receipt = st
        .reserve
        .redeem(RedeemRequest {
            from,
            amount_micros: body.amount_micros,
            payout_ref: body.payout_ref,
            kyc_tier: body.kyc_tier.into(),
            payout_dest: body.payout_dest.into(),
            idem_key: body.idem_key,
        })
        .await?;
    Ok(ok_status(
        StatusCode::CREATED,
        views::redeem_receipt(&receipt),
    ))
}
