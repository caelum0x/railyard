//! On-ramp deposit webhooks (§4), one route per provider under
//! `/v1/deposits/:provider/webhook`. Each delivery is verified by
//! `mint-payments` over the RAW body (provider-specific signature scheme) BEFORE
//! any parse, then the confirmed [`DepositEvent`] drives an idempotent
//! `reserve.mint` keyed on the `<provider>:<ref>` namespaced deposit reference.
//!
//! These routes are NOT behind the `x-api-key` middleware: providers cannot
//! attach a custom API-key header, so authenticity is the webhook signature
//! alone (verified per-provider in `mint-payments`).

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use mint_payments::{ProviderId, WebhookError};
use mint_reserve::MintRequest;
use mint_store::LedgerStore;
use serde_json::json;

use crate::dto::parse_account;
use crate::error::{ok, ApiError, ApiResult};
use crate::handlers::reversals;
use crate::state::AppState;
use crate::views;

/// Case-insensitive [`mint_payments::Headers`] view over an axum [`HeaderMap`],
/// so provider signature verification runs without pulling axum into
/// `mint-payments`.
struct AxumHeaders<'a>(&'a HeaderMap);

impl mint_payments::Headers for AxumHeaders<'_> {
    fn get_header(&self, name: &str) -> Option<&str> {
        self.0.get(name).and_then(|v| v.to_str().ok())
    }
}

/// `POST /v1/deposits/:provider/webhook` — verify + mint for any provider.
pub async fn provider_webhook<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(provider): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult {
    let provider = ProviderId::from_tag(&provider)
        .ok_or_else(|| ApiError::BadRequest(format!("unknown payment provider: {provider}")))?;

    // Payments must be configured for any webhook to be honored (BYOK).
    let registry = st
        .payments
        .as_ref()
        .ok_or_else(|| ApiError::Internal("provider not configured".into()))?;

    // Verify the RAW body against the provider signature scheme, then branch on
    // the outcome taxonomy: a verified deposit mints; an `Ignored` (well-formed
    // but non-crediting) event is acknowledged 200 so the provider stops
    // retrying; every other outcome maps to an error status.
    let event = match registry
        .verify_webhook(provider, &AxumHeaders(&headers), &body)
        .await
    {
        Ok(ev) => ev,
        Err(WebhookError::Ignored(kind)) => {
            // Not a deposit: it may be a refund / chargeback of one we already
            // minted, which must be clawed back so supply stays backed.
            return match registry.verify_reversal(provider, &AxumHeaders(&headers), &body) {
                Ok(rev) => {
                    let out = reversals::apply_reversal(
                        &st,
                        &rev.namespaced_ref(),
                        Some(rev.amount_micros),
                        rev.reason.as_str(),
                        Some(format!("{} webhook", provider.tag())),
                    )
                    .await;
                    match out {
                        Ok(o) => Ok(ok(json!({
                            "reversal": reversals::reversal_view(&o.record),
                            "burned_now_micros": o.burned_now_micros,
                        }))),
                        // A reversal of a deposit Mint never minted (e.g. a
                        // payment for something else on the same account) is
                        // acknowledged so the provider stops retrying.
                        Err(ApiError::NotFound(m)) => Ok(ok(json!({ "ignored": m }))),
                        Err(e) => Err(e),
                    }
                }
                Err(WebhookError::BadSignature) => Err(ApiError::Unauthorized),
                Err(WebhookError::Malformed(m)) => Err(ApiError::BadRequest(m)),
                Err(_) => Ok(ok(json!({ "ignored": kind }))),
            };
        }
        Err(WebhookError::BadSignature) => return Err(ApiError::Unauthorized),
        Err(WebhookError::NotConfigured) => {
            return Err(ApiError::Internal("provider not configured".into()))
        }
        Err(WebhookError::Malformed(m)) => return Err(ApiError::BadRequest(m)),
    };

    let to = parse_account(&event.account_id).map_err(ApiError::from)?;
    // The `<provider>:<ref>` namespaced key is BOTH the deposit_ref (so the
    // composite ramp routes re-confirmation to the right adapter) AND the
    // idempotency key (so a duplicate delivery mints exactly once).
    let namespaced = event.namespaced_ref();
    let receipt = st
        .reserve
        .mint(MintRequest {
            to,
            amount_micros: event.amount_micros,
            deposit_ref: namespaced.clone(),
            idem_key: namespaced,
        })
        .await?;
    Ok(ok(views::mint_receipt(&receipt)))
}
