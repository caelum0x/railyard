//! KYC onboarding: report an account's provider-verified standing and start a
//! hosted verification (Persona inquiry / Stripe Identity session) so an
//! account holder can become eligible to redeem. The redeem gate itself reads
//! the provider live, so no local KYC state is stored or trusted.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use mint_reserve::{KycError, KycStatus, KycTier};
use mint_store::LedgerStore;
use serde::Deserialize;
use serde_json::json;

use crate::dto::parse_account;
use crate::error::{ok, ok_status, ApiError, ApiResult};
use crate::handlers::checkouts::validate_redirect_url;
use crate::state::AppState;

fn kyc_err(e: KycError) -> ApiError {
    match e {
        KycError::NotConfigured => ApiError::Unavailable(
            "KYC provider is not fully configured (MINT_KYC_PROVIDER + its API key; Persona also \
             needs PERSONA_INQUIRY_TEMPLATE_ID)"
                .into(),
        ),
        KycError::ProviderUnavailable => {
            ApiError::Unavailable("KYC provider is unreachable; try again shortly".into())
        }
        KycError::MalformedResponse => {
            ApiError::Unavailable("KYC provider returned an unexpected response".into())
        }
    }
}

fn tier_name(t: KycTier) -> &'static str {
    match t {
        KycTier::None => "none",
        KycTier::Basic => "basic",
        KycTier::Verified => "verified",
    }
}

/// `GET /v1/accounts/:id/kyc` → `{ provider, status, tier }` where status is
/// `unverified | pending | approved | declined`, or `unavailable` when no KYC
/// provider is configured (redemptions are then denied for everyone).
pub async fn status<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
) -> ApiResult {
    let account = parse_account(&id).map_err(ApiError::from)?;
    st.ledger.read_balance(&account).await?;
    let Some(kyc) = st.kyc_onboarding.as_ref() else {
        return Ok(ok(json!({
            "account_id": account.as_str(),
            "provider": null,
            "status": "unavailable",
            "tier": null,
            "can_start": false,
        })));
    };
    let status = kyc.verify(&account).await.map_err(kyc_err)?;
    let (name, tier) = match status {
        KycStatus::Unverified => ("unverified", None),
        KycStatus::Pending => ("pending", None),
        KycStatus::Approved(t) => ("approved", Some(tier_name(t))),
        KycStatus::Declined => ("declined", None),
    };
    Ok(ok(json!({
        "account_id": account.as_str(),
        "provider": kyc.provider(),
        "status": name,
        "tier": tier,
        "can_start": !matches!(status, KycStatus::Approved(_)),
    })))
}

/// Body for `POST /v1/accounts/:id/kyc/session`.
#[derive(Debug, Default, Deserialize)]
pub struct StartSessionReq {
    /// Where the provider returns the user after verifying (https).
    #[serde(default)]
    pub return_url: Option<String>,
}

/// `POST /v1/accounts/:id/kyc/session` → `{ provider, session_id, url }`: a
/// hosted verification bound to the account.
pub async fn start_session<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(id): Path<String>,
    body: Option<Json<StartSessionReq>>,
) -> ApiResult {
    let account = parse_account(&id).map_err(ApiError::from)?;
    st.ledger.read_balance(&account).await?;
    let kyc = st.kyc_onboarding.as_ref().ok_or_else(|| {
        ApiError::Unavailable(
            "no KYC provider configured (set MINT_KYC_PROVIDER=persona|stripe_identity)".into(),
        )
    })?;
    let return_url = match body.and_then(|Json(b)| b.return_url) {
        Some(url) => Some(validate_redirect_url(&url)?),
        None => None,
    };
    let session = kyc
        .start_session(&account, return_url.as_deref())
        .await
        .map_err(kyc_err)?;
    Ok(ok_status(
        StatusCode::CREATED,
        json!({
            "account_id": account.as_str(),
            "provider": session.provider,
            "session_id": session.session_id,
            "url": session.url,
        }),
    ))
}
