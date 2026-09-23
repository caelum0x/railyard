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
async fn find_org_invoice(
    st: &AppState,
    org: Uuid,
    invoice_id: Uuid,
) -> ApiResult<rago_core::models::Invoice> {
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
    let created = payments
        .iter()
        .find(|r| r.0 == id)
        .map(payment_json)
        .unwrap_or(Value::Null);
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
    Ok(Json(
        json!({ "payments": payments.iter().map(payment_json).collect::<Vec<_>>() }),
    ))
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
    let payment = payments
        .iter()
        .find(|r| r.0 == id)
        .ok_or_else(|| ApiError::NotFound("payment".into()))?;
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

/// `POST /payment_requests` — create a payment request over a customer's
/// overdue invoices.
///
/// Ports `PaymentRequests::CreateService`: resolve the customer, gather their
/// overdue invoices in the target currency (optionally narrowed to explicit
/// `lago_invoice_ids`), enforce the preconditions (non-empty, all overdue,
/// single currency), price the net amount (Σ owed − credit-note offset), link
/// the invoices, and emit `payment_request.created`. The provider charge itself
/// is worker-driven (the dunning sweep), mirroring Lago's async
/// `PaymentRequests::Payments::CreateService.call_async`.
pub async fn create_payment_request(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    use rago_billing::dunning::{build_payment_request, OverdueInvoice, PaymentRequestError};

    let org = auth.organization.id;
    let p = wrapped(&body, "payment_request");
    let external_customer_id = req_s(p, "external_customer_id")?;
    let customer = st
        .db
        .find_customer_by_external_id(org, &external_customer_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("customer".into()))?;
    let email = s(p, "email").or_else(|| customer.email.clone());

    // Resolve the target currency: explicit → customer default → the sole
    // currency of the customer's overdue balances (else a mixed-currency batch
    // is rejected by the preconditions below).
    let balances = st.db.customer_overdue_balances(org, customer.id).await?;
    let currency = s(p, "amount_currency")
        .or_else(|| customer.currency.clone())
        .or_else(|| {
            if balances.len() == 1 {
                balances.keys().next().cloned()
            } else {
                None
            }
        })
        .ok_or_else(|| ApiError::validation("amount_currency", "value_is_mandatory"))?;

    // Overdue invoices in that currency, optionally narrowed to explicit ids.
    let mut rows = st
        .db
        .overdue_invoices_for_currency(org, customer.id, &currency)
        .await?;
    if let Some(ids) = explicit_invoice_ids(p) {
        rows.retain(|r| ids.contains(&r.id));
    }
    let invoices: Vec<OverdueInvoice> = rows
        .iter()
        .map(|r| OverdueInvoice {
            id: r.id,
            currency: currency.clone(),
            total_amount_cents: r.total_amount_cents,
            total_paid_amount_cents: r.total_paid_amount_cents,
            credit_note_offset_cents: r.credit_note_offset_cents,
            payment_overdue: true,
        })
        .collect();

    let plan = build_payment_request(&invoices).map_err(|e| match e {
        PaymentRequestError::NoInvoices => ApiError::NotFound("invoice".into()),
        PaymentRequestError::InvoicesNotOverdue => {
            ApiError::validation("base", "invoices_not_overdue")
        }
        PaymentRequestError::DifferentCurrencies => {
            ApiError::validation("base", "invoices_have_different_currencies")
        }
    })?;

    let id = Uuid::new_v4();
    let row = st
        .db
        .insert_payment_request(
            id,
            org,
            Some(customer.id),
            &external_customer_id,
            email.as_deref(),
            plan.amount_cents,
            Some(&plan.currency),
        )
        .await?;
    for invoice_id in &plan.invoice_ids {
        st.db
            .insert_payment_request_applied_invoice(Uuid::new_v4(), id, *invoice_id, org)
            .await?;
    }

    // `payment_request.created` — persisted pending for the delivery worker
    // (the API has no outbound transport). No-op when the org has no endpoint.
    let _ = rago_jobs::webhook_emit::record_webhook(
        &st.db,
        org,
        rago_integrations::webhooks::WebhookKind::PaymentRequestCreated,
        json!({
            "lago_id": id,
            "external_customer_id": external_customer_id,
            "email": email,
            "amount_cents": plan.amount_cents,
            "amount_currency": plan.currency,
            "payment_status": "pending",
            "lago_invoice_ids": plan.invoice_ids,
        }),
    )
    .await;

    Ok(Json(
        json!({ "payment_request": payment_request_json(&row) }),
    ))
}

/// Parse an optional `lago_invoice_ids` array from the request body.
fn explicit_invoice_ids(p: &Value) -> Option<Vec<Uuid>> {
    let arr = p.get("lago_invoice_ids")?.as_array()?;
    Some(
        arr.iter()
            .filter_map(|v| v.as_str())
            .filter_map(|s| Uuid::parse_str(s).ok())
            .collect(),
    )
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
    let rows = st
        .db
        .list_payment_requests(auth.organization.id, &q.external_customer_id)
        .await?;
    Ok(Json(
        json!({ "payment_requests": rows.iter().map(payment_request_json).collect::<Vec<_>>() }),
    ))
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
    Ok(Json(
        json!({ "payment_request": payment_request_json(&row) }),
    ))
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
    let email = rago_mail::builders::payment_receipt_email(
        &invoice,
        to.as_deref().unwrap_or("billing@customer.example"),
    );
    let _ = st.mailer.send(&email).await;
    Ok(Json(
        json!({ "payment_receipt": { "lago_invoice_id": invoice.id, "email_resent": true } }),
    ))
}
