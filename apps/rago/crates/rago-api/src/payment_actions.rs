//! Payments waiting on the customer (Stripe SCA / 3-D Secure).
//!
//! * `GET /invoices/:id/payment_action` — the pending action and its confirm
//!   link (`payment_action: null` when nothing is pending).
//! * `POST /invoices/:id/payment_action/notify` — (re)generate the confirm link
//!   when needed, queue `payment.requires_action`, and email the customer.
//! * `DELETE /invoices/:id` — delete a draft invoice (`invoice.deleted`).

use axum::extract::{Path, State};
use axum::Json;
use rago_db::payment_actions::PaymentActionRow;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::state::AppState;

fn action_json(a: &PaymentActionRow) -> Value {
    json!({
        "lago_payment_id": a.payment_id,
        "lago_invoice_id": a.invoice_id,
        "provider": a.provider,
        "provider_payment_id": a.provider_payment_id,
        "provider_status": a.provider_status,
        "status": "requires_action",
        "next_action_type": a.next_action.get("type").and_then(Value::as_str),
        "next_action": a.next_action,
        "payment_url": a.next_action_url,
    })
}

async fn org_invoice(
    st: &AppState,
    auth: &Authed,
    id: Uuid,
) -> ApiResult<rago_core::models::Invoice> {
    st.db
        .find_invoice(id)
        .await?
        .filter(|i| i.organization_id == auth.organization.id)
        .ok_or_else(|| ApiError::NotFound("invoice".into()))
}

/// `GET /invoices/:id/payment_action`.
pub async fn show_payment_action(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let invoice = org_invoice(&st, &auth, id).await?;
    let action = st.db.invoice_payment_action(invoice.id).await?;
    Ok(Json(json!({ "payment_action": action.as_ref().map(action_json) })))
}

/// `POST /invoices/:id/payment_action/notify`.
pub async fn notify_payment_action(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let invoice = org_invoice(&st, &auth, id).await?;
    if st.db.invoice_payment_action(invoice.id).await?.is_none() {
        return Err(ApiError::validation("payment_action", "no_pending_action"));
    }
    let customer = match invoice.customer_id {
        Some(cid) => st.db.find_customer(cid).await?,
        None => None,
    }
    .ok_or_else(|| ApiError::NotFound("customer".into()))?;

    let transport = rago_integrations::default_transport();
    let url = rago_jobs::collection::notify_requires_action(&st.db, &transport, &invoice, &customer)
        .await
        .map_err(|e| match e {
            rago_jobs::JobError::Db(e) => ApiError::from(e),
            other => ApiError::Validation(json!({ "payment_action": [other.to_string()] })),
        })?
        .ok_or_else(|| ApiError::validation("payment_url", "confirm_link_unavailable"))?;

    let to = customer
        .email
        .clone()
        .filter(|e| e.contains('@'))
        .ok_or_else(|| ApiError::validation("customer", "email_missing"))?;
    let email = rago_mail::builders::payment_action_email(&invoice, &url, &to);
    if let Err(e) = st.mailer.send(&email).await {
        tracing::warn!(invoice_id = %invoice.id, error = %e, "payment action email failed");
        return Err(ApiError::Validation(json!({ "email": [format!("delivery_failed: {e}")] })));
    }
    let action = st.db.invoice_payment_action(invoice.id).await?;
    Ok(Json(json!({
        "payment_action": action.as_ref().map(action_json),
        "email_sent_to": to,
    })))
}

/// `DELETE /invoices/:id` — delete a DRAFT invoice and its fees, queueing
/// `invoice.deleted`. Finalized invoices must be voided instead.
pub async fn delete_draft_invoice(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let invoice = org_invoice(&st, &auth, id).await?;
    if !matches!(invoice.status, rago_core::enums::InvoiceStatus::Draft) {
        return Err(ApiError::validation("invoice", "not_draft_void_instead"));
    }
    if !st.db.delete_draft_invoice(auth.organization.id, invoice.id).await? {
        return Err(ApiError::validation("invoice", "not_deletable"));
    }
    let object = json!({
        "lago_id": invoice.id,
        "number": invoice.number,
        "status": "deleted",
        "lago_customer_id": invoice.customer_id,
        "total_amount_cents": invoice.total_amount_cents,
        "currency": invoice.currency,
    });
    crate::emit::webhook(
        &st.db,
        auth.organization.id,
        rago_integrations::webhooks::WebhookKind::InvoiceDeleted,
        object.clone(),
    )
    .await;
    Ok(Json(json!({ "invoice": object })))
}
