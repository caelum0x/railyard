//! On-ramp deposit webhooks (§4), one route per provider under
//! `/v1/deposits/:provider/webhook`. Each delivery is verified by
//! `mint-payments` over the RAW body (provider-specific signature scheme) BEFORE
//! any parse, then the confirmed [`DepositEvent`] drives an idempotent
//! `reserve.mint` keyed on the `<provider>:<ref>` namespaced deposit reference.
//!
//! These routes are NOT behind the `x-api-key` middleware: providers cannot
//! attach a custom API-key header, so authenticity is the webhook signature
//! alone (verified per-provider in `mint-payments`).

use std::collections::HashMap;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use mint_payments::{PayoutEvent, ProviderId, ReversalEvent, VerifiedEvent, WebhookError};
use mint_reserve::MintRequest;
use mint_store::LedgerStore;
use serde_json::json;

use crate::dto::parse_account;
use crate::error::{ok, ApiError, ApiResult};
use crate::handlers::{checkouts, reversals};
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

/// Google Cloud Pub/Sub push subscriptions cannot attach a custom static
/// header, only a query parameter on the push endpoint URL — so Play's
/// webhook auth token travels as `?token=...` instead of a header. This
/// bridges it into the one synthetic header [`mint_payments::iap::playstore`]
/// checks, keeping `mint-payments` itself query-string/axum-free (its own
/// module doc explains why). A real `x-playstore-token` header, if a caller
/// sends one directly (e.g. local testing with `curl`), is used as a fallback
/// when no query token is present.
struct PlayStoreHeaders<'a> {
    inner: AxumHeaders<'a>,
    token: Option<&'a str>,
}

impl mint_payments::Headers for PlayStoreHeaders<'_> {
    fn get_header(&self, name: &str) -> Option<&str> {
        if name.eq_ignore_ascii_case("x-playstore-token") {
            if let Some(t) = self.token {
                return Some(t);
            }
        }
        self.inner.get_header(name)
    }
}

/// The two [`mint_payments::Headers`] views a delivery can need, picked once
/// per request in [`handle_webhook`] (an enum, not a trait object — `Headers`
/// has no blanket impl for `Box<dyn Headers>` and doesn't need one for just
/// two concrete shapes).
enum RequestHeaders<'a> {
    Plain(AxumHeaders<'a>),
    PlayStore(PlayStoreHeaders<'a>),
}

impl mint_payments::Headers for RequestHeaders<'_> {
    fn get_header(&self, name: &str) -> Option<&str> {
        match self {
            RequestHeaders::Plain(h) => h.get_header(name),
            RequestHeaders::PlayStore(h) => h.get_header(name),
        }
    }
}

/// What a verified webhook delivery did.
enum Outcome {
    /// A deposit was minted (or replayed idempotently).
    Minted(Response),
    /// A reversal / payout update was applied, or the event was acknowledged
    /// as not actionable.
    Handled(Response),
    /// A well-formed, authenticated event that credits nothing.
    Ignored(String),
}

/// `POST /v1/deposits/:provider/webhook` — verify + mint for any provider.
///
/// Iyzico's hosted Checkout Form returns the BUYER'S BROWSER here with a form
/// POST; that request gets a browser answer (a redirect to the success page,
/// or a readable status page) instead of the JSON envelope providers get.
pub async fn provider_webhook<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Path(provider): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let browser_callback = provider == ProviderId::Iyzico.tag() && is_form_post(&headers);
    let outcome = handle_webhook(&st, &provider, &headers, query.get("token").map(String::as_str), &body).await;
    if browser_callback {
        return browser_response(&st, outcome);
    }
    match outcome {
        Ok(Outcome::Minted(r) | Outcome::Handled(r)) => r,
        Ok(Outcome::Ignored(kind)) => ok(json!({ "ignored": kind })),
        Err(e) => e.into_response(),
    }
}

fn is_form_post(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("application/x-www-form-urlencoded"))
}

/// Answer a buyer's browser returning from a hosted checkout.
fn browser_response<S: LedgerStore + 'static>(
    st: &AppState<S>,
    outcome: Result<Outcome, ApiError>,
) -> Response {
    match outcome {
        Ok(Outcome::Minted(_)) | Ok(Outcome::Handled(_)) => {
            let target = st
                .public
                .checkout_success_url
                .clone()
                .or_else(|| {
                    st.public
                        .public_url
                        .as_ref()
                        .map(|b| format!("{b}/checkout/success"))
                })
                .unwrap_or_else(|| "/checkout/success".to_string());
            Redirect::to(&target).into_response()
        }
        Ok(Outcome::Ignored(kind)) => {
            tracing::info!(kind = %kind, "hosted checkout returned without a completed payment");
            checkouts::status_page(
                StatusCode::PAYMENT_REQUIRED,
                "Payment not completed",
                "The payment provider did not confirm this payment, so nothing was credited. You \
                 can close this page and start the deposit again.",
            )
        }
        Err(e) => {
            tracing::warn!(error = ?e, "hosted checkout callback could not be credited yet");
            checkouts::status_page(
                StatusCode::ACCEPTED,
                "Deposit pending",
                "Your payment is being confirmed with the provider. It will be credited \
                 automatically as soon as the provider's confirmation arrives; no further \
                 action is needed.",
            )
        }
    }
}

/// Verify one delivery and apply it.
async fn handle_webhook<S: LedgerStore + 'static>(
    st: &AppState<S>,
    provider: &str,
    headers: &HeaderMap,
    playstore_token: Option<&str>,
    body: &Bytes,
) -> Result<Outcome, ApiError> {
    let provider = ProviderId::from_tag(provider)
        .ok_or_else(|| ApiError::BadRequest(format!("unknown payment provider: {provider}")))?;

    // Payments must be configured for any webhook to be honored (BYOK).
    let registry = st
        .payments
        .as_ref()
        .ok_or_else(|| ApiError::Internal("provider not configured".into()))?;

    let headers_view = if provider == ProviderId::PlayStore {
        RequestHeaders::PlayStore(PlayStoreHeaders { inner: AxumHeaders(headers), token: playstore_token })
    } else {
        RequestHeaders::Plain(AxumHeaders(headers))
    };

    // Verify the RAW body against the provider signature scheme ONCE, then
    // branch on the outcome taxonomy: a verified deposit mints; a verified
    // refund / chargeback claws the minted credit back; a payout settlement
    // update moves the payout record; an `Ignored` (well-formed but
    // non-crediting) event is acknowledged 200 so the provider stops
    // retrying; every other outcome maps to an error status.
    let event = match registry.verify_event(provider, &headers_view, body).await {
        Ok(VerifiedEvent::Deposit(ev)) => ev,
        Ok(VerifiedEvent::Reversal(rev)) => {
            return apply_webhook_reversal(st, provider, &rev)
                .await
                .map(Outcome::Handled);
        }
        Ok(VerifiedEvent::Payout(update)) => {
            return apply_payout_update(st, &update).await.map(Outcome::Handled);
        }
        Err(WebhookError::Ignored(kind)) => return Ok(Outcome::Ignored(kind)),
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
    Ok(Outcome::Minted(ok(views::mint_receipt(&receipt))))
}

/// Apply a verified provider payout settlement update to the payout record.
/// An update for a payout Mint has no record of (another integration's
/// transfer) is acknowledged so the provider stops retrying.
async fn apply_payout_update<S: LedgerStore + 'static>(
    st: &AppState<S>,
    update: &PayoutEvent,
) -> ApiResult {
    let payout_ref = update.namespaced_ref();
    let applied = st
        .reserve
        .apply_payout_update(
            &payout_ref,
            update.status,
            update.provider_ref.clone(),
            &update.event_type,
        )
        .await?;
    match applied {
        Some(rec) => {
            tracing::info!(
                payout_ref = %rec.payout_ref,
                status = rec.status.as_str(),
                event = %update.event_type,
                "payout updated by provider webhook"
            );
            Ok(ok(json!({
                "payout_ref": rec.payout_ref,
                "status": rec.status.as_str(),
                "event": update.event_type,
            })))
        }
        None => Ok(ok(json!({
            "ignored": format!("no payout {payout_ref} for {}", update.event_type),
        }))),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use mint_payments::Headers as _;

    #[test]
    fn playstore_header_prefers_the_query_token_over_a_sent_header() {
        let mut raw = HeaderMap::new();
        raw.insert("x-playstore-token", "header-value".parse().unwrap());
        let h = RequestHeaders::PlayStore(PlayStoreHeaders {
            inner: AxumHeaders(&raw),
            token: Some("query-value"),
        });
        assert_eq!(h.get_header("x-playstore-token"), Some("query-value"));
        // Case-insensitive, same as every other provider's header lookup.
        assert_eq!(h.get_header("X-PlayStore-Token"), Some("query-value"));
    }

    #[test]
    fn playstore_header_falls_back_to_a_sent_header_without_a_query_token() {
        let mut raw = HeaderMap::new();
        raw.insert("x-playstore-token", "header-value".parse().unwrap());
        let h = RequestHeaders::PlayStore(PlayStoreHeaders { inner: AxumHeaders(&raw), token: None });
        assert_eq!(h.get_header("x-playstore-token"), Some("header-value"));
    }

    #[test]
    fn playstore_header_is_absent_without_either_source() {
        let raw = HeaderMap::new();
        let h = RequestHeaders::PlayStore(PlayStoreHeaders { inner: AxumHeaders(&raw), token: None });
        assert_eq!(h.get_header("x-playstore-token"), None);
    }

    #[test]
    fn plain_headers_are_untouched_by_the_playstore_bridge() {
        let mut raw = HeaderMap::new();
        raw.insert("authorization", "Bearer x".parse().unwrap());
        let h = RequestHeaders::Plain(AxumHeaders(&raw));
        assert_eq!(h.get_header("authorization"), Some("Bearer x"));
    }
}
