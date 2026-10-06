//! Provider-hosted checkout creation, bound directly to a Mint account, plus
//! the buyer-facing return surface: Mint-hosted success / cancel pages and the
//! PayPal approval return that captures the order and credits the account.

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::Json;
use mint_payments::{CheckoutUrls, PaymentError, ProviderId, RampError};
use mint_reserve::MintRequest;
use mint_store::LedgerStore;
use serde::Deserialize;
use serde_json::json;

use crate::dto::{parse_account, CheckoutReq};
use crate::error::{ok_status, ApiError, ApiResult};
use crate::state::{AppState, PublicConfig};

/// Accept only absolute `https://` redirect targets (plus `http://localhost`
/// for local development). Rejects `javascript:`, relative and plain-http URLs
/// so a checkout can never bounce a buyer to an unsafe scheme.
pub fn validate_redirect_url(raw: &str) -> Result<String, ApiError> {
    let url = raw.trim();
    let ok_scheme = url.starts_with("https://")
        || url.starts_with("http://localhost")
        || url.starts_with("http://127.0.0.1");
    let host_part = url.split("://").nth(1).unwrap_or("");
    if !ok_scheme || host_part.is_empty() || host_part.starts_with('/') || url.len() > 2048 {
        return Err(ApiError::BadRequest(format!(
            "redirect URL must be an absolute https:// URL (got '{url}')"
        )));
    }
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(ApiError::BadRequest("redirect URL contains invalid characters".into()));
    }
    Ok(url.to_string())
}

/// Resolve the buyer return URLs: request → env default → Mint's own hosted
/// pages. Errors when nothing is configured so a buyer is never sent to a dead
/// host after paying.
fn resolve_urls(public: &PublicConfig, body: &CheckoutReq) -> Result<CheckoutUrls, ApiError> {
    let hosted = |path: &str| public.public_url.as_ref().map(|base| format!("{base}{path}"));
    let pick = |req: &Option<String>, env: &Option<String>, path: &str, name: &str| {
        match req.as_deref().filter(|u| !u.trim().is_empty()) {
            Some(u) => validate_redirect_url(u),
            None => env.clone().or_else(|| hosted(path)).ok_or_else(|| {
                ApiError::Unavailable(format!(
                    "no {name} configured: pass {name} in the request, or set \
                     MINT_CHECKOUT_{}_URL or MINT_PUBLIC_URL",
                    name.trim_end_matches("_url").to_ascii_uppercase()
                ))
            }),
        }
    };
    Ok(CheckoutUrls {
        success_url: pick(
            &body.success_url,
            &public.checkout_success_url,
            "/checkout/success",
            "success_url",
        )?,
        cancel_url: pick(
            &body.cancel_url,
            &public.checkout_cancel_url,
            "/checkout/cancel",
            "cancel_url",
        )?,
        capture_return_url: hosted("/checkout/paypal/return"),
        public_base_url: public.public_url.clone(),
    })
}

fn payment_err(e: PaymentError) -> ApiError {
    match e {
        PaymentError::Misconfigured(m) => ApiError::Unavailable(m),
        PaymentError::InvalidRequest(m) => ApiError::BadRequest(m),
        PaymentError::NotConfigured(p) => {
            ApiError::BadRequest(format!("checkout provider {} is not configured", p.tag()))
        }
        PaymentError::Ramp(RampError::ProviderUnavailable) | PaymentError::Transport(_) => {
            tracing::warn!(error = %e, "checkout provider call failed");
            ApiError::Unavailable("payment provider is unavailable; try again shortly".into())
        }
        other => {
            tracing::error!(error = %other, "checkout creation failed");
            ApiError::Internal("checkout creation failed".into())
        }
    }
}

/// `POST /v1/checkouts` → a provider-hosted checkout crediting `account_id`.
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
        || matches!(provider, ProviderId::RevenueCat | ProviderId::Adapty | ProviderId::AppStore)
    {
        return Err(ApiError::BadRequest(format!(
            "checkout provider {} is not active",
            provider.tag()
        )));
    }
    let urls = resolve_urls(&state.public, &body)?;
    let checkout = payments
        .create_checkout(provider, account.as_str(), body.amount_micros, &urls)
        .await
        .map_err(payment_err)?;
    let checkout_id = checkout.checkout_id;
    // The mint key is `<provider>:<ref>` where `ref` is what the provider's
    // crediting webhook carries. For PayPal / Creem that IS the checkout id; for
    // Stripe (PaymentIntent), Polar (order) and Dodo (payment_id; the checkout
    // id is only a session handle) the ref only exists once paid,
    // so no deposit_ref is promised up front. Iyzico credits under the
    // `paymentId`, which is only assigned once the buyer pays (the checkout id
    // is the form token).
    let deposit_ref = match provider {
        ProviderId::Stripe | ProviderId::Polar | ProviderId::Iyzico | ProviderId::Dodo => None,
        _ => Some(provider.namespaced(&checkout_id)),
    };
    Ok(ok_status(
        StatusCode::CREATED,
        json!({
            "provider": provider.tag(),
            "checkout_id": checkout_id,
            "checkout_url": checkout.checkout_url,
            "deposit_ref": deposit_ref,
            "account_id": account.as_str(),
            "amount_micros": body.amount_micros,
            "success_url": urls.success_url,
            "cancel_url": urls.cancel_url,
        }),
    ))
}

/// PayPal appends `?token=<order id>&PayerID=…` to the approval return URL.
#[derive(Debug, Deserialize)]
pub struct PayPalReturn {
    token: Option<String>,
}

/// `GET /checkout/paypal/return?token=<order id>` (public; the buyer's browser
/// lands here after approving on PayPal). Captures the order server-side,
/// mints under `paypal:<order id>` — the same key the
/// `PAYMENT.CAPTURE.COMPLETED` webhook uses, so exactly one credit happens —
/// then redirects to the success page. The credit is bound to the order's
/// authenticated `custom_id`, so a forged return cannot redirect funds.
pub async fn paypal_return<S: LedgerStore + 'static>(
    State(st): State<AppState<S>>,
    Query(q): Query<PayPalReturn>,
) -> Response {
    let Some(order_id) = q.token.filter(|t| !t.is_empty()) else {
        return page(
            StatusCode::BAD_REQUEST,
            "Payment not found",
            "The payment return link is missing its order reference.",
        );
    };
    let Some(payments) = st.payments.as_ref() else {
        return page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Payments unavailable",
            "PayPal is not configured on this Mint deployment.",
        );
    };
    let deposit = match payments.capture_paypal_order(&order_id).await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!(order_id = %order_id, error = %e, "paypal capture did not complete");
            return page(
                StatusCode::PAYMENT_REQUIRED,
                "Payment not completed",
                "PayPal did not confirm this payment. You have not been charged; you can close \
                 this page and start the deposit again.",
            );
        }
    };
    let Some(account) = deposit
        .bound_account
        .as_deref()
        .and_then(|a| parse_account(a).ok())
    else {
        tracing::error!(order_id = %order_id, "paypal order missing custom_id account binding");
        return page(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Deposit needs attention",
            "Your payment was received but could not be matched to an account. Support has the \
             reference below.",
        );
    };
    let deposit_ref = ProviderId::PayPal.namespaced(&order_id);
    let minted = st
        .reserve
        .mint(MintRequest {
            to: account,
            amount_micros: deposit.amount_micros.get(),
            deposit_ref: deposit_ref.clone(),
            idem_key: deposit_ref.clone(),
        })
        .await;
    match minted {
        Ok(_) => {
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
        Err(e) => {
            tracing::error!(order_id = %order_id, error = %e, "paypal deposit captured but mint failed");
            page(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Deposit pending",
                "Your payment was captured and will be credited automatically once PayPal's \
                 confirmation arrives. No further action is needed.",
            )
        }
    }
}

/// `GET /checkout/success` — Mint-hosted landing page after paying.
pub async fn success_page() -> Response {
    page(
        StatusCode::OK,
        "Payment received",
        "Thank you. Your deposit is being credited to your balance; it appears as soon as the \
         payment provider confirms it, usually within a minute. You can close this page.",
    )
}

/// `GET /checkout/cancel` — Mint-hosted landing page after abandoning.
pub async fn cancel_page() -> Response {
    page(
        StatusCode::OK,
        "Checkout cancelled",
        "No payment was taken. You can close this page and start a new deposit at any time.",
    )
}

/// A Mint-hosted buyer status page (shared with the hosted-checkout callbacks
/// on the deposit webhook route).
pub(crate) fn status_page(status: StatusCode, title: &str, message: &str) -> Response {
    page(status, title, message)
}

/// A small self-contained status page (no external assets, no script).
fn page(status: StatusCode, title: &str, message: &str) -> Response {
    let html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>{title} - Mint</title><style>\
         body{{margin:0;min-height:100vh;display:grid;place-items:center;\
         font:16px/1.5 system-ui,-apple-system,Segoe UI,sans-serif;background:#0b0d10;color:#e6e8eb}}\
         main{{max-width:28rem;padding:2rem;border:1px solid #23272e;border-radius:12px;background:#12151a}}\
         h1{{font-size:1.25rem;margin:0 0 .5rem}}p{{margin:0;color:#a8b0ba}}\
         </style></head><body><main><h1>{title}</h1><p>{message}</p></main></body></html>",
        title = html_escape(title),
        message = html_escape(message),
    );
    (status, Html(html)).into_response()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}
