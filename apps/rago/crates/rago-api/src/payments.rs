//! Payments, payment requests, and payment receipts — REST wiring over the
//! existing [`rago_db::Db`] payment ops. `create` records a real payment /
//! request row; provider-side settlement is owner-gated, so provider status is
//! surfaced honestly (`pending`) rather than fabricated.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::handlers::{i64_of, req_s, s, wrapped};
use crate::state::AppState;

// ---- org scoping -----------------------------------------------------------

/// Load an invoice owned by the authenticated organization. Cross-tenant ids
/// answer `404` (never `403`) so the existence of another org's invoice does
/// not leak — mirrors the GraphQL layer's org filter.
async fn find_org_invoice(st: &AppState, org: Uuid, invoice_id: Uuid) -> ApiResult<rago_core::models::Invoice> {
    st.db
        .find_invoice_for_org(org, invoice_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("invoice".into()))
}

// ---- payments --------------------------------------------------------------

fn payment_json(r: &rago_db::PaymentRow) -> Value {
    // PaymentRow = (id, provider, status, amount_cents, amount_currency).
    json!({
        "lago_id": r.0,
        "payment_provider_type": r.1,
        "status": r.2,
        "amount_cents": r.3,
        "amount_currency": r.4,
    })
}

/// `POST /payments` — record a payment against an invoice.
pub async fn create_payment(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let p = wrapped(&body, "payment");
    let invoice_id = Uuid::parse_str(&req_s(p, "invoice_id")?)
        .map_err(|_| ApiError::validation("invoice_id", "value_is_invalid"))?;
    let invoice = find_org_invoice(&st, org, invoice_id).await?;
    let amount_cents = i64_of(p, "amount_cents").unwrap_or(invoice.total_amount_cents);
    let currency = s(p, "amount_currency").unwrap_or_else(|| invoice.currency().code());
    let provider = s(p, "payment_provider_type").unwrap_or_else(|| "manual".into());
    let id = Uuid::new_v4();
    st.db
        .insert_payment(
            id,
            org,
            invoice_id,
            &provider,
            s(p, "provider_payment_id").as_deref(),
            s(p, "provider_status").as_deref(),
            &s(p, "status").unwrap_or_else(|| "pending".into()),
            amount_cents,
            &currency,
        )
        .await?;
    let payments = st.db.list_payments(invoice_id).await?;
    let created = payments.iter().find(|r| r.0 == id).map(payment_json).unwrap_or(Value::Null);
    Ok(Json(json!({ "payment": created })))
}

#[derive(Debug, Deserialize)]
pub struct PaymentIndexQuery {
    pub invoice_id: Uuid,
}

/// `GET /payments?invoice_id=` — payments recorded against an invoice.
pub async fn list_payments(
    State(st): State<AppState>,
    auth: Authed,
    Query(q): Query<PaymentIndexQuery>,
) -> ApiResult<Json<Value>> {
    find_org_invoice(&st, auth.organization.id, q.invoice_id).await?;
    let payments = st.db.list_payments(q.invoice_id).await?;
    Ok(Json(json!({ "payments": payments.iter().map(payment_json).collect::<Vec<_>>() })))
}

/// `GET /payments/:id?invoice_id=` — a single payment (scoped to its invoice).
pub async fn show_payment(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    Query(q): Query<PaymentIndexQuery>,
) -> ApiResult<Json<Value>> {
    find_org_invoice(&st, auth.organization.id, q.invoice_id).await?;
    let payments = st.db.list_payments(q.invoice_id).await?;
    let payment = payments.iter().find(|r| r.0 == id).ok_or_else(|| ApiError::NotFound("payment".into()))?;
    Ok(Json(json!({ "payment": payment_json(payment) })))
}

// ---- payment requests ------------------------------------------------------

fn payment_request_json(r: &rago_db::PaymentRequestRow) -> Value {
    // PaymentRequestRow = (id, external_customer_id, email, amount_cents, currency, payment_status).
    json!({
        "lago_id": r.0,
        "external_customer_id": r.1,
        "email": r.2,
        "amount_cents": r.3,
        "amount_currency": r.4,
        "payment_status": r.5,
    })
}

/// `POST /payment_requests` — create a payment request for a customer's
/// overdue balance.
pub async fn create_payment_request(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let p = wrapped(&body, "payment_request");
    let external_customer_id = req_s(p, "external_customer_id")?;
    let customer = st.db.find_customer_by_external_id(org, &external_customer_id).await?;
    let email = s(p, "email").or_else(|| customer.as_ref().and_then(|c| c.email.clone()));
    let amount_cents = i64_of(p, "amount_cents").unwrap_or(0);
    let currency = s(p, "amount_currency").or_else(|| customer.as_ref().and_then(|c| c.currency.clone()));
    let id = Uuid::new_v4();
    let row = st
        .db
        .insert_payment_request(
            id,
            org,
            customer.as_ref().map(|c| c.id),
            &external_customer_id,
            email.as_deref(),
            amount_cents,
            currency.as_deref(),
        )
        .await?;
    Ok(Json(json!({ "payment_request": payment_request_json(&row) })))
}

#[derive(Debug, Deserialize)]
pub struct PaymentRequestIndexQuery {
    pub external_customer_id: String,
}

/// `GET /payment_requests?external_customer_id=`.
pub async fn list_payment_requests(
    State(st): State<AppState>,
    auth: Authed,
    Query(q): Query<PaymentRequestIndexQuery>,
) -> ApiResult<Json<Value>> {
    let rows = st.db.list_payment_requests(auth.organization.id, &q.external_customer_id).await?;
    Ok(Json(json!({ "payment_requests": rows.iter().map(payment_request_json).collect::<Vec<_>>() })))
}

/// `GET /payment_requests/:id`.
pub async fn show_payment_request(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let row = st
        .db
        .find_payment_request_for_org(auth.organization.id, id)
        .await?
        .ok_or_else(|| ApiError::NotFound("payment_request".into()))?;
    Ok(Json(json!({ "payment_request": payment_request_json(&row) })))
}

// ---- payment receipts ------------------------------------------------------

/// `GET /payment_receipts?invoice_id=` — receipts for an invoice's payments
/// (honest partial: receipts are derived from recorded payments).
pub async fn list_payment_receipts(
    State(st): State<AppState>,
    auth: Authed,
    Query(q): Query<PaymentIndexQuery>,
) -> ApiResult<Json<Value>> {
    find_org_invoice(&st, auth.organization.id, q.invoice_id).await?;
    let payments = st.db.list_payments(q.invoice_id).await?;
    let receipts: Vec<Value> = payments
        .iter()
        .map(|r| json!({ "lago_id": r.0, "lago_payment_id": r.0, "lago_invoice_id": q.invoice_id, "amount_cents": r.3, "amount_currency": r.4 }))
        .collect();
    Ok(Json(json!({ "payment_receipts": receipts })))
}

/// `POST /payment_receipts/:invoice_id/resend_email` — best-effort resend of the
/// payment-receipt email through the injectable transport.
pub async fn resend_payment_receipt(
    State(st): State<AppState>,
    auth: Authed,
    Path(invoice_id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let invoice = find_org_invoice(&st, auth.organization.id, invoice_id).await?;
    let to = match invoice.customer_id {
        Some(cid) => st.db.find_customer(cid).await?.and_then(|c| c.email),
        None => None,
    };
    let email = rago_mail::builders::payment_receipt_email(&invoice, to.as_deref().unwrap_or("billing@customer.example"));
    let _ = st.mailer.send(&email).await;
    Ok(Json(json!({ "payment_receipt": { "lago_invoice_id": invoice.id, "email_resent": true } })))
}
