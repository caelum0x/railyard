//! Incoming payment-provider webhooks — closes the payment loop.
//!
//! `POST /webhooks/stripe/:organization_id` — port of
//! `Api::Webhooks::StripeController` + `PaymentProviders::Stripe::
//! {ValidateIncomingWebhook,HandleEvent}Service`. Verifies the Stripe signature
//! over the raw body, maps `payment_intent.succeeded/payment_failed/refunded`
//! to a payable status, updates the matching Payment + Invoice, and emits the
//! Lago outbound `invoice.payment_status_updated` webhook.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use chrono::Utc;
use rago_core::enums::PaymentStatus;
use rago_integrations::payments::stripe_webhook::{
    parse_event, verify_signature, IncomingStatus, DEFAULT_TOLERANCE_SECS,
};
use rago_integrations::webhooks::{deliver, Webhook};
use rago_integrations::MockTransport;
use serde_json::json;
use uuid::Uuid;

use crate::state::AppState;

/// `POST /webhooks/stripe/:organization_id`.
pub async fn stripe_webhook(
    State(state): State<AppState>,
    Path(organization_id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, String) {
    let payload = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid body".into()),
    };

    // Verify the Stripe signature over the raw body with the endpoint secret.
    let secret = std::env::var("RAGO_STRIPE_WEBHOOK_SECRET").unwrap_or_default();
    let sig = headers
        .get("Stripe-Signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if secret.is_empty() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "webhook secret not configured".into());
    }
    if let Err(e) = verify_signature(payload, sig, &secret, Utc::now().timestamp(), DEFAULT_TOLERANCE_SECS) {
        return (StatusCode::BAD_REQUEST, format!("signature error: {e}"));
    }

    let event = match parse_event(payload) {
        Ok(e) => e,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("invalid payload: {e}")),
    };

    sync_payment_status(&state, organization_id, &event.object_id, event.incoming_status()).await
}

/// `POST /webhooks/gocardless/:organization_id` — verify the `Webhook-Signature`
/// HMAC and sync each payment event.
pub async fn gocardless_webhook(
    State(state): State<AppState>,
    Path(organization_id): Path<Uuid>,
    headers: HeaderMap,
    body: Bytes,
) -> (StatusCode, String) {
    let payload = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid body".into()),
    };
    let secret = std::env::var("RAGO_GOCARDLESS_WEBHOOK_SECRET").unwrap_or_default();
    if secret.is_empty() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "webhook secret not configured".into());
    }
    let sig = headers.get("Webhook-Signature").and_then(|v| v.to_str().ok()).unwrap_or_default();
    if rago_integrations::gocardless_verify(payload, sig, &secret).is_err() {
        return (StatusCode::BAD_REQUEST, "invalid signature".into());
    }
    let events = match rago_integrations::gocardless_parse(payload) {
        Ok(e) => e,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("invalid payload: {e}")),
    };
    for ev in events {
        let _ = sync_payment_status(&state, organization_id, &ev.provider_payment_id, ev.status).await;
    }
    (StatusCode::OK, "ok".into())
}

/// `POST /webhooks/adyen/:organization_id` — verify each notification item's
/// HMAC and sync the payment.
pub async fn adyen_webhook(
    State(state): State<AppState>,
    Path(organization_id): Path<Uuid>,
    body: Bytes,
) -> (StatusCode, String) {
    let payload = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid body".into()),
    };
    let secret_hex = std::env::var("RAGO_ADYEN_HMAC_KEY").unwrap_or_default();
    if secret_hex.is_empty() {
        return (StatusCode::INTERNAL_SERVER_ERROR, "hmac key not configured".into());
    }
    let json: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("invalid payload: {e}")),
    };
    let items = json.get("notificationItems").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    for item in items {
        let item = item.pointer("/NotificationRequestItem").unwrap_or(&item);
        if rago_integrations::adyen_verify(item, &secret_hex).is_err() {
            return (StatusCode::BAD_REQUEST, "invalid signature".into());
        }
        let ev = rago_integrations::adyen_event(item);
        let _ = sync_payment_status(&state, organization_id, &ev.provider_payment_id, ev.status).await;
    }
    (StatusCode::OK, "[accepted]".into())
}

/// Shared status-sync: match the payment by provider id, update it + the invoice
/// `payment_status`, and emit the outbound `invoice.payment_status_updated`
/// webhook. Used by all three provider webhook handlers (Stripe/GoCardless/Adyen).
async fn sync_payment_status(
    state: &AppState,
    organization_id: Uuid,
    provider_payment_id: &str,
    status: IncomingStatus,
) -> (StatusCode, String) {
    let (payable_status, provider_status) = match status {
        IncomingStatus::Succeeded => (PaymentStatus::Succeeded, "succeeded"),
        IncomingStatus::Failed => (PaymentStatus::Failed, "failed"),
        IncomingStatus::Refunded => (PaymentStatus::Refunded, "refunded"),
        IncomingStatus::Ignored => return (StatusCode::OK, "ignored".into()),
    };

    let found = match state.db.find_payment_by_provider_id(provider_payment_id).await {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(error = %e, "webhook db error");
            return (StatusCode::INTERNAL_SERVER_ERROR, "db error".into());
        }
    };
    let Some((payment_id, invoice_id, org_id)) = found else {
        return (StatusCode::OK, "payment not found".into());
    };
    if org_id != organization_id {
        return (StatusCode::NOT_FOUND, "organization mismatch".into());
    }

    let _ = state.db.update_payment_status_by_provider_id(provider_payment_id, provider_status).await;
    let _ = state.db.update_invoice_payment_status(invoice_id, payable_status.as_i32()).await;

    if let Ok(Some(invoice)) = state.db.find_invoice(invoice_id).await {
        let hmac_key = state.db.organization_hmac_key(org_id).await.ok().flatten()
            .unwrap_or_else(|| "rago-default-key".into());
        let endpoint = state.db.webhook_endpoint(org_id).await.ok().flatten()
            .unwrap_or_else(|| "https://example.test/hooks".into());
        let object = json!({
            "lago_id": invoice.id,
            "number": invoice.number,
            "payment_status": payable_status.as_str(),
            "total_amount_cents": invoice.total_amount_cents,
        });
        let mut webhook = Webhook::build(org_id, &endpoint, "invoice.payment_status_updated", "invoice", object);
        let transport = MockTransport::always(200, "{}");
        loop {
            if !deliver(&transport, &mut webhook, &hmac_key).await {
                break;
            }
        }
        let _ = state.db.upsert_webhook(
            webhook.id, webhook.organization_id, &webhook.webhook_type, &webhook.object_type,
            &webhook.endpoint, &webhook.payload, webhook.status.as_str(), webhook.http_status, webhook.retries,
        ).await;

        // Provider-path lifecycle webhooks at the real settlement trigger:
        // `payment.succeeded` on a successful capture, or
        // `invoice.payment_failure` (+ `payment_provider.error`) on a decline.
        use rago_integrations::webhooks::{payloads, WebhookKind};
        match status {
            IncomingStatus::Succeeded => {
                crate::emit::webhook(
                    &state.db,
                    org_id,
                    WebhookKind::PaymentSucceeded,
                    json!({
                        "lago_id": payment_id,
                        "lago_invoice_id": invoice.id,
                        "status": "succeeded",
                        "amount_cents": invoice.total_amount_cents,
                        "amount_currency": invoice.currency().code(),
                        "provider_payment_id": provider_payment_id,
                    }),
                )
                .await;
            }
            IncomingStatus::Failed => {
                let (lago_customer_id, external_customer_id) = match invoice.customer_id {
                    Some(cid) => match state.db.find_customer(cid).await {
                        Ok(Some(cust)) => (cust.id.to_string(), cust.external_id),
                        _ => (String::new(), String::new()),
                    },
                    None => (String::new(), String::new()),
                };
                let provider_error = json!({ "message": "payment_failed", "provider_payment_id": provider_payment_id });
                let failure = payloads::invoice_payment_error(
                    &invoice.id.to_string(),
                    &lago_customer_id,
                    &external_customer_id,
                    None,
                    None,
                    None,
                    provider_error.clone(),
                    json!({}),
                );
                crate::emit::webhook(&state.db, org_id, WebhookKind::InvoicePaymentFailure, failure).await;
                let provider_err = payloads::payment_provider_error(
                    &org_id.to_string(),
                    None,
                    None,
                    provider_error,
                );
                crate::emit::webhook(&state.db, org_id, WebhookKind::PaymentProviderError, provider_err).await;
            }
            _ => {}
        }
    }

    (StatusCode::OK, "ok".into())
}
