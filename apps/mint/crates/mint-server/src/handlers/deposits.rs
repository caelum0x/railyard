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
use mint_payments::{ProviderId, ReversalEvent, VerifiedEvent, WebhookError};
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

    // Verify the RAW body against the provider signature scheme ONCE, then
    // branch on the outcome taxonomy: a verified deposit mints; a verified
    // refund / chargeback claws the minted credit back; an `Ignored`
    // (well-formed but non-crediting) event is acknowledged 200 so the
    // provider stops retrying; every other outcome maps to an error status.
    let event = match registry
        .verify_event(provider, &AxumHeaders(&headers), &body)
        .await
    {
        Ok(VerifiedEvent::Deposit(ev)) => ev,
        Ok(VerifiedEvent::Reversal(rev)) => {
            return apply_webhook_reversal(&st, provider, &rev).await;
        }
        Err(WebhookError::Ignored(kind)) => return Ok(ok(json!({ "ignored": kind }))),
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

/// Claw back a verified provider refund / chargeback. The original deposit is
/// looked up under each candidate `<provider>:<ref>` key in turn (providers
/// can credit one payment under a checkout OR an order / subscription id);
/// the first one Mint actually minted is reversed. A reversal of a payment
/// Mint never minted (e.g. a charge for something else) is acknowledged so
/// the provider stops retrying.
async fn apply_webhook_reversal<S: LedgerStore + 'static>(
    st: &AppState<S>,
    provider: ProviderId,
    rev: &ReversalEvent,
) -> ApiResult {
    let candidates = rev.namespaced_refs();
    for deposit_ref in &candidates {
        let out = reversals::apply_reversal(
            st,
            deposit_ref,
            rev.amount_micros,
            rev.reason.as_str(),
            Some(format!("{} webhook", provider.tag())),
        )
        .await;
        match out {
            Ok(o) => {
                return Ok(ok(json!({
                    "reversal": reversals::reversal_view(&o.record),
                    "burned_now_micros": o.burned_now_micros,
                })))
            }
            Err(ApiError::NotFound(_)) => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(ok(json!({
        "ignored": format!(
            "no minted deposit for {} reversal of {}",
            provider.tag(),
            candidates.join(", ")
        ),
    })))
}
