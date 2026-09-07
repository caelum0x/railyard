//! Provider-hosted checkout creation, bound directly to a Mint account.

use axum::extract::State;
use axum::http::StatusCode;
use axum::Json;
use mint_payments::ProviderId;
use mint_store::LedgerStore;
use serde_json::json;

use crate::dto::{parse_account, CheckoutReq};
use crate::error::{ok_status, ApiError, ApiResult};
use crate::state::AppState;

pub async fn create<S: LedgerStore + 'static>(
    State(state): State<AppState<S>>,
    Json(body): Json<CheckoutReq>,
) -> ApiResult {
    if body.amount_micros <= 0 || body.amount_micros % 10_000 != 0 {
        return Err(ApiError::BadRequest(
            "amount_micros must be a positive whole number of USD cents".into(),
        ));
    }
    let account = parse_account(&body.account_id).map_err(ApiError::from)?;
    // Existence check before creating a payment that could not be credited.
    state.ledger.read_balance(&account).await?;
    let provider = ProviderId::from_tag(&body.provider)
        .ok_or_else(|| ApiError::BadRequest("unknown checkout provider".into()))?;
    let payments = state
        .payments
        .as_ref()
        .ok_or_else(|| ApiError::BadRequest("checkout providers are not configured".into()))?;
    if !payments.is_active(provider)
        || matches!(provider, ProviderId::RevenueCat | ProviderId::Adapty)
    {
        return Err(ApiError::BadRequest(format!(
            "checkout provider {} is not active",
            provider.tag()
        )));
    }
    let checkout = payments
        .create_checkout(provider, account.as_str(), body.amount_micros)
        .await
        .map_err(|error| ApiError::Internal(error.to_string()))?;
    let checkout_id = checkout.checkout_id;
    let checkout_url = checkout.checkout_url;
    let deposit_ref = provider.namespaced(&checkout_id);
    Ok(ok_status(
        StatusCode::CREATED,
        json!({
            "provider": provider.tag(),
            "checkout_id": checkout_id,
            "checkout_url": checkout_url,
            "deposit_ref": deposit_ref,
            "account_id": account.as_str(),
            "amount_micros": body.amount_micros,
        }),
    ))
}
