//! Incoming webhooks for the hosted-checkout providers: Creem, Dodo Payments
//! and Iyzico.
//!
//! * `POST /webhooks/creem/:organization_id`: `creem-signature` (hex
//!   HMAC-SHA256 of the raw body). `checkout.completed` settles the invoice
//!   payment created under that checkout id. `refund.created` settles the
//!   refund Rago issued (keyed by the Creem transaction id).
//! * `POST /webhooks/dodo/:organization_id`: Standard Webhooks signature with
//!   a five-minute replay window. `payment.succeeded` / `payment.failed` settle
//!   the payment and move it from its checkout session id to Dodo's
//!   `payment_id`. `refund.succeeded` / `refund.failed` settle refunds.
//! * `POST /webhooks/iyzico/:organization_id`: either a server-to-server
//!   merchant notification signed with `X-IYZ-SIGNATURE-V3`, or the buyer's
//!   browser returning from the hosted form with a `token`. The token is never
//!   trusted as is. It is resolved through Iyzico's signed checkout-form detail
//!   call, and the browser is then redirected to the provider's return URL.
//!
//! Each delivery id is recorded per organization, so a replayed webhook is
//! acknowledged but not applied twice. Status changes go through the same
//! monotonic sync as Stripe, so a late failure never downgrades a paid invoice.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use chrono::Utc;
use rago_integrations::payments::stripe_webhook::IncomingStatus;
use rago_integrations::{PayableStatus, ProviderRefundEvent};
use uuid::Uuid;

use crate::state::AppState;
use crate::webhooks::{apply_refund, sync_payment_status, webhook_secrets};

type Reply = (StatusCode, String);

fn db_error(e: impl std::fmt::Display) -> Reply {
    tracing::error!(error = %e, "checkout webhook db error");
    (StatusCode::INTERNAL_SERVER_ERROR, "db error".into())
}

fn header_str<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or_default()
}

/// Record the delivery id; `Some(reply)` means it was already processed.
async fn dedupe(
    state: &AppState,
    organization_id: Uuid,
    provider: &str,
    event_id: Option<&str>,
    event_type: &str,
) -> Result<Option<Reply>, Reply> {
    let Some(event_id) = event_id.filter(|id| !id.is_empty()) else {
        return Ok(None);
    };
    let fresh = state
        .db
        .record_provider_webhook_event(organization_id, provider, event_id, event_type)
        .await
        .map_err(db_error)?;
    Ok((!fresh).then(|| (StatusCode::OK, "duplicate delivery".to_string())))
}

/// Let the provider's retry through again when applying the event failed.
async fn release(state: &AppState, organization_id: Uuid, provider: &str, event_id: Option<&str>) {
    if let Some(id) = event_id.filter(|id| !id.is_empty()) {
        if let Err(e) = state.db.forget_provider_webhook_event(organization_id, provider, id).await {
            tracing::warn!(error = %e, provider, "could not release webhook delivery id");
        }
    }
}

/// Settle a hosted-checkout payment. `references` are the ids the payment may
/// be stored under (settled id first, then checkout / session / token),
/// `settled_id` the provider's final payment id to bind it to, and
/// `lago_invoice_id` the invoice from checkout metadata as a last resort.
async fn settle(
    state: &AppState,
    organization_id: Uuid,
    references: &[&str],
    settled_id: Option<&str>,
    lago_invoice_id: Option<&str>,
    status: IncomingStatus,
) -> Reply {
    if status == IncomingStatus::Ignored {
        return (StatusCode::OK, "ignored".into());
    }
    let mut found = None;
    for reference in references.iter().filter(|r| !r.is_empty()) {
        match state.db.find_payment_by_provider_reference(organization_id, reference).await {
            Ok(Some(p)) => {
                found = Some(p);
                break;
            }
            Ok(None) => {}
            Err(e) => return db_error(e),
        }
    }
    if found.is_none() {
        if let Some(invoice_id) = lago_invoice_id.and_then(|s| Uuid::parse_str(s).ok()) {
            let payment_id = rago_jobs::payments::deterministic_payment_id(invoice_id);
            match state.db.payment_attempt_state(payment_id).await {
                Ok(Some(_)) => {
                    if let Some(settled) = settled_id {
                        if let Err(e) = state
                            .db
                            .bind_checkout_to_settled_payment(organization_id, payment_id, settled)
                            .await
                        {
                            return db_error(e);
                        }
                        match state.db.find_payment_by_provider_reference(organization_id, settled).await {
                            Ok(p) => found = p,
                            Err(e) => return db_error(e),
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => return db_error(e),
            }
        }
    }
    let Some(payment) = found else {
        return (StatusCode::OK, "payment not found".into());
    };
    if payment.organization_id != organization_id {
        return (StatusCode::NOT_FOUND, "organization mismatch".into());
    }
    let mut current = payment.provider_payment_id.clone().unwrap_or_default();
    if let Some(settled) = settled_id.filter(|s| !s.is_empty() && *s != current) {
        match state
            .db
            .bind_checkout_to_settled_payment(organization_id, payment.payment_id, settled)
            .await
        {
            Ok(_) => current = settled.to_string(),
            Err(e) => return db_error(e),
        }
    }
    let reply = sync_payment_status(state, organization_id, &current, status).await;
    if status == IncomingStatus::Succeeded && reply.0 == StatusCode::OK {
        if let Err(e) = state.db.clear_payment_next_action_by_provider_id(&current).await {
            tracing::warn!(error = %e, "checkout webhook: could not clear next_action");
        }
    }
    reply
}

async fn settle_refund(state: &AppState, organization_id: Uuid, ev: &ProviderRefundEvent) -> Result<bool, Reply> {
    apply_refund(state, organization_id, ev).await
}

// ---- Creem ------------------------------------------------------------------

/// `POST /webhooks/creem/:organization_id`.
pub async fn creem_webhook(
    State(state): State<AppState>,
    Path(organization_id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Reply {
    let Ok(payload) = std::str::from_utf8(&body) else {
        return (StatusCode::BAD_REQUEST, "invalid body".into());
    };
    let secrets = match webhook_secrets(&state, organization_id, "creem", "RAGO_CREEM_WEBHOOK_SECRET").await {
        Ok(s) => s,
        Err(e) => return e,
    };
    let sig = header_str(&headers, "creem-signature");
    if !secrets.iter().any(|s| rago_integrations::creem_verify(payload, sig, s).is_ok()) {
        return (StatusCode::BAD_REQUEST, "invalid signature".into());
    }
    let event = match rago_integrations::creem_parse(payload) {
        Ok(e) => e,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("invalid payload: {e}")),
    };
    match dedupe(&state, organization_id, "creem", event.event_id.as_deref(), &event.event_type).await {
        Ok(Some(dup)) => return dup,
        Ok(None) => {}
        Err(e) => return e,
    }
    let reply = handle_creem(&state, organization_id, &event).await;
    if reply.0.is_server_error() {
        release(&state, organization_id, "creem", event.event_id.as_deref()).await;
    }
    reply
}

async fn handle_creem(state: &AppState, organization_id: Uuid, event: &rago_integrations::CreemWebhook) -> Reply {
    if let Some(refund) = &event.refund {
        return match settle_refund(state, organization_id, refund).await {
            Ok(true) => (StatusCode::OK, "refund updated".into()),
            Ok(false) => {
                // Issued from the Creem dashboard: Creem refunds whole
                // payments, so a successful one marks the payment refunded.
                match (&event.refund_checkout_id, refund.state) {
                    (Some(checkout), rago_integrations::RefundState::Succeeded) => {
                        settle(state, organization_id, &[checkout], None, None, IncomingStatus::Refunded).await
                    }
                    _ => (StatusCode::OK, "refund not found".into()),
                }
            }
            Err(e) => e,
        };
    }
    match &event.payment {
        Some(p) => {
            settle(state, organization_id, &[&p.provider_payment_id], None, None, p.status).await
        }
        None => (StatusCode::OK, "ignored".into()),
    }
}

// ---- Dodo -------------------------------------------------------------------

/// `POST /webhooks/dodo/:organization_id`.
pub async fn dodo_webhook(
    State(state): State<AppState>,
    Path(organization_id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Reply {
    let Ok(payload) = std::str::from_utf8(&body) else {
        return (StatusCode::BAD_REQUEST, "invalid body".into());
    };
    let secrets = match webhook_secrets(&state, organization_id, "dodo", "RAGO_DODO_WEBHOOK_SECRET").await {
        Ok(s) => s,
        Err(e) => return e,
    };
    let id = header_str(&headers, "webhook-id").to_string();
    let ts = header_str(&headers, "webhook-timestamp");
    let sig = header_str(&headers, "webhook-signature");
    let now = Utc::now().timestamp();
    let mut last = None;
    let verified = secrets.iter().any(|s| match rago_integrations::dodo_verify(payload, &id, ts, sig, s, now) {
        Ok(()) => true,
        Err(e) => {
            last = Some(e);
            false
        }
    });
    if !verified {
        let msg = last.map(|e| e.to_string()).unwrap_or_else(|| "signature mismatch".into());
        return (StatusCode::BAD_REQUEST, format!("signature error: {msg}"));
    }
    let event = match rago_integrations::dodo_parse(payload) {
        Ok(e) => e,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("invalid payload: {e}")),
    };
    match dedupe(&state, organization_id, "dodo", Some(&id), &event.event_type).await {
        Ok(Some(dup)) => return dup,
        Ok(None) => {}
        Err(e) => return e,
    }
    let reply = handle_dodo(&state, organization_id, &event).await;
    if reply.0.is_server_error() {
        release(&state, organization_id, "dodo", Some(&id)).await;
    }
    reply
}

async fn handle_dodo(state: &AppState, organization_id: Uuid, event: &rago_integrations::DodoWebhook) -> Reply {
    if let Some(refund) = &event.refund {
        return match settle_refund(state, organization_id, refund).await {
            Ok(true) => (StatusCode::OK, "refund updated".into()),
            Ok(false) => (StatusCode::OK, "refund not found".into()),
            Err(e) => e,
        };
    }
    let Some(p) = &event.payment else {
        return (StatusCode::OK, "ignored".into());
    };
    let session = event.checkout_session_id.as_deref().unwrap_or_default();
    settle(
        state,
        organization_id,
        &[&p.provider_payment_id, session],
        Some(&p.provider_payment_id),
        event.lago_invoice_id.as_deref(),
        p.status,
    )
    .await
}

// ---- Iyzico -----------------------------------------------------------------

/// `POST /webhooks/iyzico/:organization_id`.
pub async fn iyzico_webhook(
    State(state): State<AppState>,
    Path(organization_id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Ok(payload) = std::str::from_utf8(&body) else {
        return (StatusCode::BAD_REQUEST, "invalid body").into_response();
    };
    if headers.contains_key("x-iyz-signature-v3") {
        return iyzico_notification(&state, organization_id, &headers, payload).await.into_response();
    }
    iyzico_browser_return(&state, organization_id, payload).await
}

async fn iyzico_notification(state: &AppState, organization_id: Uuid, headers: &HeaderMap, payload: &str) -> Reply {
    let secrets = match webhook_secrets(state, organization_id, "iyzico", "RAGO_IYZICO_SECRET_KEY").await {
        Ok(s) => s,
        Err(e) => return e,
    };
    let sig = header_str(headers, "x-iyz-signature-v3");
    let Some(n) = secrets
        .iter()
        .find_map(|s| rago_integrations::iyzico_verify_notification(payload, sig, s).ok())
    else {
        return (StatusCode::BAD_REQUEST, "invalid signature".into());
    };
    let event_id = format!("{}:{}:{}", n.event_type, n.payment_id, n.status);
    match dedupe(state, organization_id, "iyzico", Some(&event_id), &n.event_type).await {
        Ok(Some(dup)) => return dup,
        Ok(None) => {}
        Err(e) => return e,
    }
    let status = n.incoming_status();
    let reply = if status == IncomingStatus::Refunded {
        iyzico_refund_notification(state, organization_id, &n.payment_id).await
    } else {
        let token = n.token.clone().unwrap_or_default();
        settle(
            state,
            organization_id,
            &[&n.payment_id, &token],
            Some(&n.payment_id).filter(|p| !p.is_empty()).map(String::as_str),
            None,
            status,
        )
        .await
    };
    if reply.0.is_server_error() {
        release(state, organization_id, "iyzico", Some(&event_id)).await;
    }
    reply
}

/// Refunds Rago issued settle synchronously; a refund notification only marks
/// the payment refunded when it was issued outside Rago (merchant panel).
async fn iyzico_refund_notification(state: &AppState, organization_id: Uuid, payment_id: &str) -> Reply {
    let payment = match state.db.find_payment_by_provider_reference(organization_id, payment_id).await {
        Ok(Some(p)) => p,
        Ok(None) => return (StatusCode::OK, "payment not found".into()),
        Err(e) => return db_error(e),
    };
    match state.db.refundable_payment(payment.invoice_id).await {
        Ok(Some(r)) if r.refunded_cents > 0 => (StatusCode::OK, "refund already recorded".into()),
        Ok(_) => settle(state, organization_id, &[payment_id], None, None, IncomingStatus::Refunded).await,
        Err(e) => db_error(e),
    }
}

/// The buyer's browser returning from the hosted form (`token=...`).
async fn iyzico_browser_return(state: &AppState, organization_id: Uuid, payload: &str) -> Response {
    let form = parse_form(payload);
    let Some(token) = form.iter().find(|(k, _)| k == "token").map(|(_, v)| v.clone()).filter(|t| !t.is_empty())
    else {
        return (StatusCode::BAD_REQUEST, "missing token").into_response();
    };
    let provider = match rago_jobs::collection::resolve_provider_of_kind(&state.db, organization_id, "iyzico").await {
        Ok(Some(p)) => p,
        Ok(None) => return (StatusCode::NOT_FOUND, "iyzico is not configured").into_response(),
        Err(e) => return db_error(e).into_response(),
    };
    let transport = rago_integrations::default_transport();
    let Some(iyzico) = rago_jobs::collection::iyzico_provider(&provider, organization_id, &transport) else {
        return (StatusCode::NOT_FOUND, "iyzico is not configured").into_response();
    };
    let paid = match iyzico.retrieve_checkout(&token).await {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "iyzico checkout lookup failed");
            return return_page(provider.success_redirect_url.as_deref(), false, "We could not confirm this payment yet. If you were charged, it will be confirmed shortly.");
        }
    };
    // The form was created by Rago for exactly one invoice payment.
    let payment = match state.db.find_payment_by_provider_reference(organization_id, &token).await {
        Ok(p) => p,
        Err(e) => return db_error(e).into_response(),
    };
    let belongs = payment
        .as_ref()
        .is_some_and(|p| paid.basket_id.as_deref() == Some(p.invoice_id.to_string().as_str()));
    if payment.is_some() && !belongs {
        return (StatusCode::BAD_REQUEST, "checkout does not match the invoice").into_response();
    }
    let status = match paid.status {
        PayableStatus::Succeeded => IncomingStatus::Succeeded,
        PayableStatus::Failed => IncomingStatus::Failed,
        PayableStatus::Pending => IncomingStatus::Ignored,
    };
    let settled = (!paid.payment_id.is_empty()).then_some(paid.payment_id.as_str());
    let reply = settle(state, organization_id, &[&token], settled, paid.basket_id.as_deref(), status).await;
    if reply.0.is_server_error() {
        return reply.into_response();
    }
    let ok = paid.status == PayableStatus::Succeeded;
    let message = if ok {
        "Payment received. Thank you."
    } else if paid.status == PayableStatus::Failed {
        "The payment did not go through. No money was taken."
    } else {
        "Your payment is being processed."
    };
    return_page(provider.success_redirect_url.as_deref(), ok, message)
}

/// Redirect the buyer to the configured return URL, or show a plain result
/// page when none is configured.
fn return_page(redirect: Option<&str>, ok: bool, message: &str) -> Response {
    if let Some(url) = redirect.filter(|u| u.starts_with("https://")) {
        let sep = if url.contains('?') { '&' } else { '?' };
        let target = format!("{url}{sep}payment={}", if ok { "succeeded" } else { "pending" });
        return (StatusCode::SEE_OTHER, [(header::LOCATION, target)]).into_response();
    }
    let body = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" \
         content=\"width=device-width,initial-scale=1\"><title>Payment</title><style>body{{font:16px \
         system-ui,sans-serif;max-width:32rem;margin:15vh auto;padding:0 1.5rem;color:#111}}</style>\
         </head><body><h1>Payment</h1><p>{}</p></body></html>",
        html_escape(message)
    );
    (StatusCode::OK, Html(body)).into_response()
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Decode an `application/x-www-form-urlencoded` body.
pub(crate) fn parse_form(body: &str) -> Vec<(String, String)> {
    body.split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (percent_decode(k), percent_decode(v))
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                match hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(b) => {
                        out.push(b);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_decoding_handles_percent_and_plus() {
        let form = parse_form("token=abc%2Fdef&status=success&name=a+b&bad=%zz");
        assert_eq!(form[0], ("token".into(), "abc/def".into()));
        assert_eq!(form[2], ("name".into(), "a b".into()));
        assert_eq!(form[3], ("bad".into(), "%zz".into()));
    }

    #[test]
    fn return_page_redirects_only_to_https() {
        let r = return_page(Some("https://app.test/billing"), true, "ok");
        assert_eq!(r.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            r.headers().get(header::LOCATION).unwrap(),
            "https://app.test/billing?payment=succeeded"
        );
        let r = return_page(Some("javascript:alert(1)"), true, "<ok>");
        assert_eq!(r.status(), StatusCode::OK);
    }
}
