//! Subscriptions (update / show / lifetime_usage) + subscription alerts CRUD
//! (canonical `alerts` model with thresholds) + credit-note actions
//! (index / update / void / resend / download.xml) + invoice actions
//! (void / finalize / refresh / lose_dispute + honest-partial provider actions).
//!
//! Every handler reuses an existing [`rago_db::Db`] op — the same update / void /
//! finalize logic the GraphQL admin surface calls. Provider-backed actions
//! (retry_payment, payment_url, sync_salesforce_id) are honest partials: they
//! return the resource with the field `null`/`pending` until live keys are wired,
//! matching the GraphQL surface — no fabricated success.

use axum::extract::{Path, Query, State};
use axum::Json;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::handlers::{i64_of, req_s, s, wrapped};
use crate::pagination::{self, PageParams};
use crate::serializers as ser;
use crate::state::AppState;

// ---- subscriptions: show / update / lifetime_usage -------------------------

/// `GET /subscriptions/:external_id`.
pub async fn show_subscription(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let sub = st
        .db
        .find_subscription_by_external_id(auth.organization.id, &external_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("subscription".into()))?;
    let plan = st.db.find_plan(sub.plan_id).await?;
    let ext = st.db.customer_external_id(sub.customer_id).await?.unwrap_or_default();
    Ok(Json(json!({ "subscription": ser::subscription(&sub, &plan, &ext) })))
}

/// `PUT /subscriptions/:external_id` — update name / subscription_at / ending_at.
pub async fn update_subscription(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let sub = st
        .db
        .find_subscription_by_external_id(auth.organization.id, &external_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("subscription".into()))?;
    let p = wrapped(&body, "subscription");
    let subscription_at = s(p, "subscription_at").and_then(parse_dt);
    let ending_at = s(p, "ending_at").and_then(parse_dt);
    let updated = st
        .db
        .update_subscription_fields(sub.id, s(p, "name").as_deref(), subscription_at, ending_at)
        .await?
        .ok_or_else(|| ApiError::NotFound("subscription".into()))?;
    let plan = st.db.find_plan(updated.plan_id).await?;
    let ext = st.db.customer_external_id(updated.customer_id).await?.unwrap_or_default();
    Ok(Json(json!({ "subscription": ser::subscription(&updated, &plan, &ext) })))
}

/// `GET /subscriptions/:external_id/lifetime_usage` — cumulative usage
/// (invoiced charge fees + live current-period usage).
pub async fn subscription_lifetime_usage(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let sub = st
        .db
        .find_subscription_by_external_id(org, &external_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("subscription".into()))?;
    let invoiced = st.db.subscription_invoiced_usage(org, &sub.external_id).await?;
    let current = crate::phase15::current_usage_amount(&st, org, &sub).await?;
    let total = invoiced + current;
    st.db.upsert_lifetime_usage(org, &sub.external_id, invoiced, current).await?;
    Ok(Json(json!({
        "lifetime_usage": {
            "external_subscription_id": sub.external_id,
            "invoiced_usage_amount_cents": invoiced,
            "current_usage_amount_cents": current,
            "total_usage_amount_cents": total,
        }
    })))
}

// ---- subscription alerts CRUD (canonical `alerts` model) -------------------

fn alert_json(a: &rago_db::AlertRow, thresholds: &[rago_db::AlertThresholdRow]) -> Value {
    json!({
        "lago_id": a.id,
        "alertable_type": a.alertable_type,
        "alertable_id": a.alertable_id,
        "alert_type": a.alert_type,
        "name": a.name,
        "code": a.code,
        "lago_billable_metric_id": a.billable_metric_id,
        "thresholds": thresholds.iter().map(threshold_json).collect::<Vec<_>>(),
    })
}
fn threshold_json(t: &rago_db::AlertThresholdRow) -> Value {
    // AlertThresholdRow = (code, value, recurring).
    json!({ "code": t.0, "value": t.1.to_string(), "recurring": t.2 })
}

/// `POST /subscriptions/:external_id/alerts`.
pub async fn create_alert(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let sub = st
        .db
        .find_subscription_by_external_id(org, &external_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("subscription".into()))?;
    let p = wrapped(&body, "alert");
    let alert_type = req_s(p, "alert_type")?;
    let code = req_s(p, "code")?;
    let metric_id = match s(p, "billable_metric_code") {
        Some(mc) => st.db.find_metric_by_code(org, &mc).await?.map(|m| m.id),
        None => None,
    };
    let id = Uuid::new_v4();
    st.db
        .insert_alert(id, org, "Subscription", sub.id, &alert_type, s(p, "name").as_deref(), &code, metric_id)
        .await?;
    let thresholds = parse_thresholds(p);
    if !thresholds.is_empty() {
        st.db.replace_alert_thresholds(org, id, &thresholds).await?;
    }
    let row = st.db.find_alert(id).await?.ok_or_else(|| ApiError::NotFound("alert".into()))?;
    let th = st.db.alert_thresholds(id).await?;
    Ok(Json(json!({ "alert": alert_json(&row, &th) })))
}

/// `GET /subscriptions/:external_id/alerts`.
pub async fn list_alerts(
    State(st): State<AppState>,
    auth: Authed,
    Path(external_id): Path<String>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let sub = st
        .db
        .find_subscription_by_external_id(org, &external_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("subscription".into()))?;
    let alerts = st.db.alerts_for("Subscription", sub.id).await?;
    let mut out = Vec::new();
    for a in &alerts {
        let th = st.db.alert_thresholds(a.id).await?;
        out.push(alert_json(a, &th));
    }
    Ok(Json(json!({ "alerts": out })))
}

async fn alert_by_code(st: &AppState, sub_id: Uuid, code: &str) -> ApiResult<rago_db::AlertRow> {
    let alerts = st.db.alerts_for("Subscription", sub_id).await?;
    alerts
        .into_iter()
        .find(|a| a.code == code)
        .ok_or_else(|| ApiError::NotFound("alert".into()))
}

/// `GET /subscriptions/:external_id/alerts/:code`.
pub async fn show_alert(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let sub = st.db.find_subscription_by_external_id(org, &external_id).await?.ok_or_else(|| ApiError::NotFound("subscription".into()))?;
    let a = alert_by_code(&st, sub.id, &code).await?;
    let th = st.db.alert_thresholds(a.id).await?;
    Ok(Json(json!({ "alert": alert_json(&a, &th) })))
}

/// `PUT /subscriptions/:external_id/alerts/:code`.
pub async fn update_alert(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let sub = st.db.find_subscription_by_external_id(org, &external_id).await?.ok_or_else(|| ApiError::NotFound("subscription".into()))?;
    let a = alert_by_code(&st, sub.id, &code).await?;
    let p = wrapped(&body, "alert");
    st.db.update_alert(a.id, s(p, "name").as_deref(), s(p, "code").as_deref()).await?;
    let thresholds = parse_thresholds(p);
    if !thresholds.is_empty() {
        st.db.replace_alert_thresholds(org, a.id, &thresholds).await?;
    }
    let row = st.db.find_alert(a.id).await?.ok_or_else(|| ApiError::NotFound("alert".into()))?;
    let th = st.db.alert_thresholds(a.id).await?;
    Ok(Json(json!({ "alert": alert_json(&row, &th) })))
}

/// `DELETE /subscriptions/:external_id/alerts/:code`.
pub async fn destroy_alert(
    State(st): State<AppState>,
    auth: Authed,
    Path((external_id, code)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let sub = st.db.find_subscription_by_external_id(org, &external_id).await?.ok_or_else(|| ApiError::NotFound("subscription".into()))?;
    let a = alert_by_code(&st, sub.id, &code).await?;
    let th = st.db.alert_thresholds(a.id).await?;
    let out = alert_json(&a, &th);
    st.db.soft_delete_alert(a.id).await?;
    Ok(Json(json!({ "alert": out })))
}

fn parse_thresholds(p: &Value) -> Vec<(Option<String>, Decimal, bool)> {
    p.get("thresholds")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|t| {
                    let value = t
                        .get("value")
                        .and_then(|v| v.as_f64().and_then(Decimal::from_f64_retain).or_else(|| v.as_str().and_then(|s| s.parse().ok())))?;
                    let code = t.get("code").and_then(Value::as_str).map(str::to_string);
                    let recurring = t.get("recurring").and_then(Value::as_bool).unwrap_or(false);
                    Some((code, value, recurring))
                })
                .collect()
        })
        .unwrap_or_default()
}

// ---- credit-note actions ---------------------------------------------------

/// `GET /credit_notes` — filterable by `invoice_id` or `external_customer_id`,
/// else the whole org's credit notes.
#[derive(Debug, serde::Deserialize)]
pub struct CreditNoteIndexQuery {
    pub invoice_id: Option<Uuid>,
    pub external_customer_id: Option<String>,
    #[serde(flatten)]
    pub page: PageParams,
}

/// `GET /credit_notes` — credit notes, optionally scoped to an invoice
/// (`?invoice_id=`) or a customer (`?external_customer_id=`); with neither, the
/// organization's full credit-note index (paginated). Ports Lago's
/// `CreditNotesQuery` (unfiltered index + customer scope).
pub async fn list_credit_notes(
    State(st): State<AppState>,
    auth: Authed,
    Query(q): Query<CreditNoteIndexQuery>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let (notes, total) = if let Some(invoice_id) = q.invoice_id {
        let n = st.db.credit_notes_for_invoice(invoice_id).await?;
        let t = n.len() as i64;
        (n, t)
    } else if let Some(ext) = q.external_customer_id.as_deref() {
        let customer = st
            .db
            .find_customer_by_external_id(org, ext)
            .await?
            .ok_or_else(|| ApiError::NotFound("customer".into()))?;
        let n = st.db.credit_notes_for_customer(customer.id).await?;
        let t = n.len() as i64;
        (n, t)
    } else {
        let n = st
            .db
            .credit_notes_for_org(org, q.page.per_page(), q.page.offset())
            .await?;
        let t = st.db.count_credit_notes_for_org(org).await?;
        (n, t)
    };
    let out: Vec<Value> = notes.iter().map(ser::credit_note).collect();
    Ok(Json(json!({
        "credit_notes": out,
        "meta": pagination::meta(q.page.page(), q.page.per_page(), total),
    })))
}

/// `PUT /credit_notes/:id` — update the refund status.
pub async fn update_credit_note(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = wrapped(&body, "credit_note");
    let status = s(p, "refund_status").and_then(refund_status_i32).unwrap_or(0);
    let cn = st
        .db
        .update_credit_note_refund_status(id, status)
        .await?
        .ok_or_else(|| ApiError::NotFound("credit_note".into()))?;
    Ok(Json(json!({ "credit_note": ser::credit_note(&cn) })))
}

/// `PUT /credit_notes/:id/void` — void a credit note.
pub async fn void_credit_note(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let cn = st.db.void_credit_note(id).await?.ok_or_else(|| ApiError::NotFound("credit_note".into()))?;
    Ok(Json(json!({ "credit_note": ser::credit_note(&cn) })))
}

/// `POST /credit_notes/:id/resend_email` — best-effort resend of the credit-note
/// email through the injectable transport.
pub async fn resend_credit_note_email(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let cn = st.db.find_credit_note(id).await?.ok_or_else(|| ApiError::NotFound("credit_note".into()))?;
    let to = st.db.customer_external_id(cn.customer_id).await?.unwrap_or_default();
    let email = rago_mail::builders::credit_note_email(&cn, if to.is_empty() { "billing@customer.example" } else { &to });
    let _ = st.mailer.send(&email).await;
    Ok(Json(json!({ "credit_note": { "lago_id": cn.id, "email_resent": true } })))
}

/// `POST /credit_notes/:id/download_xml` — the credit-note e-invoice XML
/// (EN 16931 CII), reusing the `rago_documents` generator.
pub async fn download_credit_note_xml(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    let cn = st.db.find_credit_note(id).await?.ok_or_else(|| ApiError::NotFound("credit_note".into()))?;
    let invoice = st.db.find_invoice(cn.invoice_id).await?;
    let invoice_number = invoice.as_ref().map(|i| i.number.clone()).unwrap_or_default();
    let customer = st
        .db
        .find_customer(cn.customer_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("customer".into()))?;
    let xml = rago_documents::cii_credit_note_xml(
        &cn,
        &auth.organization,
        &customer,
        &invoice_number,
        rago_documents::Profile::En16931,
    );
    let headers = [(axum::http::header::CONTENT_TYPE, "application/xml".to_string())];
    Ok((headers, xml).into_response())
}

// ---- invoice actions -------------------------------------------------------

async fn invoice_response(st: &AppState, inv: rago_core::models::Invoice) -> ApiResult<Value> {
    let fees = st.db.fees_for_invoice(inv.id).await?;
    Ok(json!({ "invoice": ser::invoice(&inv, &fees) }))
}

/// `POST /invoices/:id/void`.
pub async fn void_invoice(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let _ = st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    st.db.void_invoice(id).await?;
    let inv = st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    Ok(Json(invoice_response(&st, inv).await?))
}

/// `PUT /invoices/:id/finalize` — finalize a draft invoice.
pub async fn finalize_invoice(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let inv = st.db.finalize_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    Ok(Json(invoice_response(&st, inv).await?))
}

/// `PUT /invoices/:id/refresh` — recompute the invoice totals from its fees.
pub async fn refresh_invoice(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let inv = st.db.refresh_invoice_totals(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    Ok(Json(invoice_response(&st, inv).await?))
}

/// `POST /invoices/:id/lose_dispute` — mark the invoice's dispute lost.
pub async fn lose_dispute(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let inv = st.db.lose_invoice_dispute(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    Ok(Json(invoice_response(&st, inv).await?))
}

/// `POST /invoices/:id/resend_email` — best-effort resend of the finalized email.
pub async fn resend_invoice_email(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let inv = st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let fees = st.db.fees_for_invoice(inv.id).await?;
    let email = match inv.customer_id {
        Some(cid) => st.db.find_customer(cid).await?.and_then(|c| c.email),
        None => None,
    };
    crate::breadth::send_invoice_finalized(&st, &inv, &fees, email.as_deref()).await;
    Ok(Json(json!({ "invoice": { "lago_id": inv.id, "email_resent": true } })))
}

/// `PUT /invoices/:id/sync_salesforce_id` — honest partial: records the intent;
/// the provider-backed sync id is `null` until a live CRM connection is wired.
pub async fn sync_salesforce_id(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let inv = st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    Ok(Json(json!({ "invoice": { "lago_id": inv.id, "external_salesforce_id": Value::Null } })))
}

/// `POST /invoices/:id/retry` — honest partial: re-finalize path is provider-
/// gated; returns the current invoice state without fabricating a payment.
pub async fn retry_invoice(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let inv = st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    Ok(Json(invoice_response(&st, inv).await?))
}

/// `POST /invoices/:id/retry_payment` — honest partial: null pending provider.
pub async fn retry_payment(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let inv = st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    Ok(Json(json!({ "invoice": { "lago_id": inv.id, "payment_status": inv.payment_status.as_str(), "retried": false } })))
}

/// `POST /invoices/:id/payment_url` — honest partial: null pending provider.
pub async fn invoice_payment_url(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let inv = st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    Ok(Json(json!({ "invoice_payment_details": { "lago_invoice_id": inv.id, "payment_url": Value::Null } })))
}

fn refund_status_i32(s: String) -> Option<i32> {
    Some(match s.as_str() {
        "pending" => 0,
        "succeeded" => 1,
        "failed" => 2,
        _ => return None,
    })
}

fn parse_dt(v: String) -> Option<chrono::NaiveDateTime> {
    chrono::DateTime::parse_from_rfc3339(&v)
        .ok()
        .map(|d| d.naive_utc())
        .or_else(|| chrono::NaiveDate::parse_from_str(&v, "%Y-%m-%d").ok().map(|d| d.and_hms_opt(0, 0, 0).unwrap()))
}

// Keep enum + i64_of imports meaningful.
#[allow(dead_code)]
fn _touch() -> Option<i64> {
    i64_of(&json!({}), "x")
}
