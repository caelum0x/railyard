//! Incoming payment-provider webhooks — closes the payment loop.
//!
//! `POST /webhooks/stripe/:organization_id` — port of
//! `Api::Webhooks::StripeController` + `PaymentProviders::Stripe::
//! {ValidateIncomingWebhook,HandleEvent}Service`. Verifies the Stripe signature
//! over the raw body, maps `payment_intent.succeeded/payment_failed/refunded`
//! to a payable status, updates the matching Payment + Invoice, and emits the
//! Lago outbound `invoice.payment_status_updated` webhook.
//!
//! Signing secrets are per organization: each org's payment provider row
//! stores its own sealed webhook secret (Stripe `whsec_…`, GoCardless endpoint
//! secret, Adyen HMAC key), looked up by the `:organization_id` in the route.
//! The `RAGO_STRIPE_WEBHOOK_SECRET` / `RAGO_GOCARDLESS_WEBHOOK_SECRET` /
//! `RAGO_ADYEN_HMAC_KEY` env vars remain a single-tenant fallback used only when
//! the org has no stored secret.
//!
//! Refund notifications (Stripe `refund.*`, GoCardless `refunds` events, Adyen
//! `REFUND`) settle the refunds Rago issued (see `rago_jobs::refunds`).

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use chrono::Utc;
use rago_core::enums::PaymentStatus;
use rago_integrations::payments::stripe_webhook::{
    parse_event, verify_signature, IncomingStatus, StripeEvent, StripeIntentProgress,
    DEFAULT_TOLERANCE_SECS,
};
use serde_json::json;
use uuid::Uuid;

use crate::state::AppState;

/// The org's stored webhook secrets for `provider`, else the env fallback.
pub(crate) async fn webhook_secrets(
    state: &AppState,
    organization_id: Uuid,
    provider: &str,
    env_var: &str,
) -> Result<Vec<String>, (StatusCode, String)> {
    let mut secrets = state
        .db
        .provider_webhook_secrets(organization_id, provider)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "webhook secret lookup failed");
            (StatusCode::INTERNAL_SERVER_ERROR, "db error".to_string())
        })?;
    if secrets.is_empty() {
        if let Some(v) = std::env::var(env_var).ok().filter(|v| !v.trim().is_empty()) {
            secrets.push(v);
        }
    }
    if secrets.is_empty() {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("{provider} webhook secret not configured for this organization"),
        ));
    }
    Ok(secrets)
}

/// Settle a refund Rago issued from a provider notification.
pub(crate) async fn apply_refund(
    state: &AppState,
    organization_id: Uuid,
    ev: &rago_integrations::ProviderRefundEvent,
) -> Result<bool, (StatusCode, String)> {
    rago_jobs::refunds::apply_refund_notification(
        &state.db,
        organization_id,
        &ev.provider_refund_id,
        ev.state,
        &ev.provider_status,
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "refund notification failed");
        (StatusCode::INTERNAL_SERVER_ERROR, "db error".to_string())
    })
}

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

    // Verify the Stripe signature over the raw body with the org's endpoint
    // secret(s).
    let secrets = match webhook_secrets(&state, organization_id, "stripe", "RAGO_STRIPE_WEBHOOK_SECRET").await {
        Ok(s) => s,
        Err(e) => return e,
    };
    let sig = headers
        .get("Stripe-Signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let now = Utc::now().timestamp();
    let mut last_err = None;
    let verified = secrets.iter().any(|secret| match verify_signature(payload, sig, secret, now, DEFAULT_TOLERANCE_SECS) {
        Ok(()) => true,
        Err(e) => {
            last_err = Some(e);
            false
        }
    });
    if !verified {
        let msg = last_err.map(|e| e.to_string()).unwrap_or_else(|| "signature mismatch".into());
        return (StatusCode::BAD_REQUEST, format!("signature error: {msg}"));
    }

    let event = match parse_event(payload) {
        Ok(e) => e,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("invalid payload: {e}")),
    };

    if let Some(refund) = event.refund_event() {
        return match apply_refund(&state, organization_id, &refund).await {
            Ok(true) => (StatusCode::OK, "refund updated".into()),
            Ok(false) => (StatusCode::OK, "refund not found".into()),
            Err(e) => e,
        };
    }

    // SCA / 3DS progress: record the customer action (or `processing`).
    if let Some(progress) = event.intent_progress() {
        return apply_intent_progress(&state, organization_id, &event, progress).await;
    }

    let incoming = event.incoming_status();
    if incoming == IncomingStatus::Succeeded {
        // A Checkout payment (the 3DS / "pay now" fallback) settles through a
        // new PaymentIntent: bind it to the invoice's payment first.
        if let Err(resp) = bind_checkout_payment(&state, organization_id, &event).await {
            return resp;
        }
    }
    let result = sync_payment_status(&state, organization_id, &event.object_id, incoming).await;
    if incoming == IncomingStatus::Succeeded && result.0 == StatusCode::OK {
        if let Err(e) = state.db.clear_payment_next_action_by_provider_id(&event.object_id).await {
            tracing::warn!(error = %e, "webhook: could not clear payment next_action");
        }
    }
    result
}

/// `payment_intent.requires_action` → store the action + confirm link and
/// queue `payment.requires_action`; `payment_intent.processing` → record it.
async fn apply_intent_progress(
    state: &AppState,
    organization_id: Uuid,
    event: &StripeEvent,
    progress: StripeIntentProgress,
) -> (StatusCode, String) {
    match progress {
        StripeIntentProgress::Processing => {
            match state.db.set_payment_provider_status(&event.object_id, "processing").await {
                Ok(true) => (StatusCode::OK, "processing recorded".into()),
                Ok(false) => (StatusCode::OK, "payment not found".into()),
                Err(e) => {
                    tracing::error!(error = %e, "webhook: processing update failed");
                    (StatusCode::INTERNAL_SERVER_ERROR, "db error".into())
                }
            }
        }
        StripeIntentProgress::RequiresAction { next_action } => {
            let url = rago_integrations::payments::stripe::next_action_redirect_url(&next_action);
            let row = match state
                .db
                .mark_payment_requires_action(&event.object_id, &next_action, url.as_deref())
                .await
            {
                Ok(Some(r)) => r,
                Ok(None) => return (StatusCode::OK, "payment not found".into()),
                Err(e) => {
                    tracing::error!(error = %e, "webhook: requires_action update failed");
                    return (StatusCode::INTERNAL_SERVER_ERROR, "db error".into());
                }
            };
            if row.organization_id != organization_id {
                return (StatusCode::NOT_FOUND, "organization mismatch".into());
            }
            let invoice = match state.db.find_invoice(row.invoice_id).await {
                Ok(Some(i)) => i,
                Ok(None) => return (StatusCode::OK, "invoice not found".into()),
                Err(e) => {
                    tracing::error!(error = %e, "webhook: invoice reload failed");
                    return (StatusCode::INTERNAL_SERVER_ERROR, "db error".into());
                }
            };
            let customer = match invoice.customer_id {
                Some(cid) => state.db.find_customer(cid).await.ok().flatten(),
                None => None,
            };
            let transport = rago_integrations::default_transport();
            match customer {
                Some(c) => {
                    if let Err(e) = rago_jobs::collection::notify_requires_action(
                        &state.db, &transport, &invoice, &c,
                    )
                    .await
                    {
                        tracing::error!(error = %e, "webhook: requires_action notification failed");
                        return (StatusCode::INTERNAL_SERVER_ERROR, "notify error".into());
                    }
                }
                None => tracing::warn!(invoice_id = %invoice.id, "requires_action on invoice without customer"),
            }
            (StatusCode::OK, "requires_action recorded".into())
        }
    }
}

/// Bind a succeeded PaymentIntent that Rago did not create (a Checkout
/// session for the invoice) to the invoice's payment row, so the settlement
/// below finds it. No-op for PaymentIntents Rago already knows.
async fn bind_checkout_payment(
    state: &AppState,
    organization_id: Uuid,
    event: &StripeEvent,
) -> Result<(), (StatusCode, String)> {
    let db_err = |e: rago_db::DbError| {
        tracing::error!(error = %e, "webhook: checkout bind failed");
        (StatusCode::INTERNAL_SERVER_ERROR, "db error".to_string())
    };
    if state.db.find_payment_by_provider_id(&event.object_id).await.map_err(db_err)?.is_some() {
        return Ok(());
    }
    let Some(invoice_id) = event.lago_invoice_id().and_then(|s| Uuid::parse_str(s).ok()) else {
        return Ok(());
    };
    let Some(invoice) = state.db.find_invoice(invoice_id).await.map_err(db_err)? else {
        return Ok(());
    };
    if invoice.organization_id != organization_id {
        return Err((StatusCode::NOT_FOUND, "organization mismatch".into()));
    }
    let payment_id = rago_jobs::payments::deterministic_payment_id(invoice.id);
    let existing = state.db.payment_attempt_state(payment_id).await.map_err(db_err)?;
    match existing {
        Some(_) => {
            state
                .db
                .rebind_invoice_payment(payment_id, invoice.id, &event.object_id)
                .await
                .map_err(db_err)?;
        }
        None => {
            state
                .db
                .record_payment_attempt(
                    payment_id,
                    invoice.organization_id,
                    invoice.id,
                    "stripe",
                    Some(&event.object_id),
                    Some("processing"),
                    "pending",
                    invoice.total_amount_cents,
                    &invoice.currency().code(),
                    0,
                    None,
                )
                .await
                .map_err(db_err)?;
        }
    }
    Ok(())
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
    let secrets = match webhook_secrets(&state, organization_id, "gocardless", "RAGO_GOCARDLESS_WEBHOOK_SECRET").await {
        Ok(s) => s,
        Err(e) => return e,
    };
    let sig = headers.get("Webhook-Signature").and_then(|v| v.to_str().ok()).unwrap_or_default();
    if !secrets.iter().any(|secret| rago_integrations::gocardless_verify(payload, sig, secret).is_ok()) {
        return (StatusCode::BAD_REQUEST, "invalid signature".into());
    }
    match rago_integrations::gocardless_refund_events(payload) {
        Ok(refunds) => {
            for ev in refunds {
                if let Err(e) = apply_refund(&state, organization_id, &ev).await {
                    return e;
                }
            }
        }
        Err(e) => return (StatusCode::BAD_REQUEST, format!("invalid payload: {e}")),
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
    let keys = match webhook_secrets(&state, organization_id, "adyen", "RAGO_ADYEN_HMAC_KEY").await {
        Ok(s) => s,
        Err(e) => return e,
    };
    let json: serde_json::Value = match serde_json::from_str(payload) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("invalid payload: {e}")),
    };
    let items = json.get("notificationItems").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    for item in items {
        let item = item.pointer("/NotificationRequestItem").unwrap_or(&item);
        if !keys.iter().any(|k| rago_integrations::adyen_verify(item, k).is_ok()) {
            return (StatusCode::BAD_REQUEST, "invalid signature".into());
        }
        if let Some(refund) = rago_integrations::adyen_refund_event(item) {
            match apply_refund(&state, organization_id, &refund).await {
                Ok(true) => continue,
                Ok(false) => {
                    // A refund issued outside Rago (Adyen Customer Area): only a
                    // successful refund marks the original payment refunded.
                    if refund.state == rago_integrations::RefundState::Succeeded {
                        if let Some(original) = rago_integrations::adyen_original_reference(item) {
                            let _ = sync_payment_status(&state, organization_id, &original, IncomingStatus::Refunded).await;
                        }
                    }
                    continue;
                }
                Err(e) => return e,
            }
        }
        let ev = rago_integrations::adyen_event(item);
        let _ = sync_payment_status(&state, organization_id, &ev.provider_payment_id, ev.status).await;
    }
    (StatusCode::OK, "[accepted]".into())
}

/// Shared status-sync: match the payment by provider id, update it + the invoice
/// `payment_status`, and emit the outbound `invoice.payment_status_updated`
/// webhook. Used by all three provider webhook handlers (Stripe/GoCardless/Adyen).
pub(crate) async fn sync_payment_status(
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
    let Some((payment_id, invoice_id, org_id, current_status)) = found else {
        return (StatusCode::OK, "payment not found".into());
    };
    if org_id != organization_id {
        return (StatusCode::NOT_FOUND, "organization mismatch".into());
    }

    // Monotonic-status guard: provider events can arrive late or be replayed.
    // A terminal state is never downgraded — a delayed `payment_failed` after
    // `succeeded` must not flip a paid invoice back to failed, and nothing
    // (success replay or failure) undoes a refund. `failed → succeeded` stays
    // allowed (a successful retry), as does `succeeded → refunded`.
    let downgrade = matches!(
        (current_status.as_str(), status),
        ("succeeded", IncomingStatus::Failed)
            | ("refunded", IncomingStatus::Failed)
            | ("refunded", IncomingStatus::Succeeded)
    );
    if downgrade {
        return (StatusCode::OK, "ignored (terminal payment status)".into());
    }

    if let Err(e) = state.db.update_payment_status_by_provider_id(provider_payment_id, provider_status).await {
        tracing::error!(error = %e, provider_payment_id, "webhook: payment status update failed");
        // 5xx so the provider retries the event.
        return (StatusCode::INTERNAL_SERVER_ERROR, "db error".into());
    }
    if let Err(e) = state.db.update_invoice_payment_status(invoice_id, payable_status.as_i32()).await {
        tracing::error!(error = %e, %invoice_id, "webhook: invoice payment status update failed");
        return (StatusCode::INTERNAL_SERVER_ERROR, "db error".into());
    }

    let invoice = match state.db.find_invoice(invoice_id).await {
        Ok(i) => i,
        Err(e) => {
            tracing::error!(error = %e, %invoice_id, "webhook: invoice reload failed");
            None
        }
    };
    if let Some(invoice) = invoice.as_ref() {
        if let Err(e) =
            rago_jobs::wallet_credits::on_invoice_payment_status(&state.db, invoice, payable_status).await
        {
            tracing::error!(error = %e, invoice_id = %invoice.id, "webhook: paid-credit settlement failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "settlement error".into());
        }
    }
    if payable_status == PaymentStatus::Succeeded {
        if let Err(e) = rago_jobs::receipts::issue_for_payment(&state.db, payment_id).await {
            tracing::error!(error = %e, %payment_id, "webhook: payment receipt failed");
        }
    }
    if let Some(invoice) = invoice {
        // Queue the outbound `invoice.payment_status_updated` for every
        // configured endpoint; the delivery worker signs it with the org's own
        // HMAC key and retries with backoff.
        crate::emit::webhook(
            &state.db,
            org_id,
            rago_integrations::webhooks::WebhookKind::InvoicePaymentStatusUpdated,
            json!({
                "lago_id": invoice.id,
                "number": invoice.number,
                "status": invoice.status.as_str(),
                "payment_status": payable_status.as_str(),
                "currency": invoice.currency,
                "total_amount_cents": invoice.total_amount_cents,
            }),
        )
        .await;

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
