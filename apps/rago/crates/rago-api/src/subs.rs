//! Subscriptions (update / show / lifetime_usage) + subscription alerts CRUD
//! (canonical `alerts` model with thresholds) + credit-note actions
//! (index / update / void / resend / download.xml) + invoice actions
//! (void / finalize / refresh / lose_dispute + honest-partial provider actions).
//!
//! Every handler reuses an existing [`rago_db::Db`] op — the same update / void /
//! finalize logic the GraphQL admin surface calls. Provider-backed actions
//! (retry_payment, payment_url) go through [`rago_jobs::collection`] against the
//! organization's configured payment provider.

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
    let out = ser::subscription(&updated, &plan, &ext);
    crate::emit::webhook(&st.db, auth.organization.id, rago_integrations::webhooks::WebhookKind::SubscriptionUpdated, out.clone()).await;
    Ok(Json(json!({ "subscription": out })))
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

pub(crate) fn alert_json(a: &rago_db::AlertRow, thresholds: &[rago_db::AlertThresholdRow]) -> Value {
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

pub(crate) fn parse_thresholds(p: &Value) -> Vec<(Option<String>, Decimal, bool)> {
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
    let to = st
        .db
        .find_customer(cn.customer_id)
        .await?
        .and_then(|c| c.email)
        .filter(|e| e.contains('@'))
        .ok_or_else(|| ApiError::validation("customer", "email_missing"))?;
    let email = rago_mail::builders::credit_note_email(&cn, &to);
    if let Err(e) = st.mailer.send(&email).await {
        tracing::warn!(credit_note_id = %cn.id, error = %e, "credit note email failed");
        return Err(ApiError::Validation(json!({ "email": [format!("delivery_failed: {e}")] })));
    }
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
    let credits = st.db.credit_note_credits_for_invoice(inv.id).await?;
    let mut body = ser::invoice(&inv, &fees);
    body["credits"] = ser::credit_note_credits(&inv, &credits);
    Ok(json!({ "invoice": body }))
}

/// `POST /invoices/:id/void`.
pub async fn void_invoice(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let _ = st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    st.db.void_invoice(id).await?;
    // Real-time wallets: free Mint holds still reserved for this invoice and
    // move money already captured for it back to the customer.
    let transport = rago_integrations::default_transport();
    match rago_jobs::wallet_realtime::settle_void(&st.db, &transport, id).await {
        Ok(v) if v.failed > 0 => {
            tracing::warn!(invoice_id = %id, failed = v.failed, "some real-time captures could not be reversed on void");
        }
        Ok(_) => {}
        Err(e) => tracing::warn!(error = %e, invoice_id = %id, "real-time wallet settlement failed on void"),
    }
    let inv = st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let body = invoice_response(&st, inv.clone()).await?;
    crate::emit::webhook(
        &st.db,
        inv.organization_id,
        rago_integrations::webhooks::WebhookKind::InvoiceVoided,
        body["invoice"].clone(),
    )
    .await;
    // Void the transaction at the tax provider (Anrok or Avalara) when one is
    // connected.
    if st.db.active_integration_of_type(inv.organization_id, "tax").await?.is_some() {
        st.db
            .enqueue_job(
                rago_jobs::integrations_sync::JOB_TAX_VOIDING,
                &json!({ "invoice_id": inv.id }),
                chrono::Utc::now().naive_utc(),
                5,
            )
            .await?;
    }
    Ok(Json(body))
}

/// `PUT /invoices/:id/finalize` — finalize a draft invoice.
pub async fn finalize_invoice(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let before = st.db.find_invoice(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    // Number + finalize in one transaction; `None` = not a draft (a
    // re-finalize is a no-op and emits nothing).
    let finalized = match st.db.finalize_draft(id).await? {
        Some(inv) => Some(
            // Provider taxes + collection; the webhooks below carry the full
            // REST invoice body.
            rago_jobs::finalize::on_invoice_finalized(&st.db, inv, false)
                .await
                .map_err(|e| {
                    tracing::error!(invoice_id = %id, error = %e, "post-finalization pipeline failed");
                    ApiError::Internal("invoice_finalization_failed".into())
                })?,
        ),
        None => None,
    };
    let transitioned = finalized.is_some();
    let inv = finalized.unwrap_or(before);
    let body = invoice_response(&st, inv.clone()).await?;
    if transitioned {
        crate::emit::webhook(
            &st.db,
            inv.organization_id,
            rago_integrations::webhooks::WebhookKind::InvoiceCreated,
            body["invoice"].clone(),
        )
        .await;
        let _ = rago_jobs::webhook_emit::record_invoice_generated(&st.db, inv.organization_id, inv.id, body["invoice"].clone()).await;
    }
    Ok(Json(body))
}

/// `PUT /invoices/:id/refresh` — `Invoices::RefreshDraftService`: a draft's
/// usage is re-priced from the events of its window; any other invoice is
/// returned unchanged.
pub async fn refresh_invoice(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let (_, inv) = rago_jobs::refresh_draft::refresh_draft_invoice(&st.db, id)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let inv = inv
        .filter(|i| i.organization_id == auth.organization.id)
        .ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    Ok(Json(invoice_response(&st, inv).await?))
}

/// `POST /invoices/:id/lose_dispute` — mark the invoice's dispute lost.
pub async fn lose_dispute(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let inv = st.db.lose_invoice_dispute(id).await?.ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let body = invoice_response(&st, inv.clone()).await?;
    crate::emit::webhook(
        &st.db,
        inv.organization_id,
        rago_integrations::webhooks::WebhookKind::InvoicePaymentDisputeLost,
        json!({ "invoice": body["invoice"].clone(), "provider_error": Value::Null }),
    )
    .await;
    Ok(Json(body))
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
    match crate::breadth::send_invoice_finalized(&st, &inv, &fees, email.as_deref()).await {
        crate::breadth::EmailOutcome::Sent => Ok(Json(json!({ "invoice": { "lago_id": inv.id, "email_resent": true } }))),
        crate::breadth::EmailOutcome::NoRecipient => Err(ApiError::validation("customer", "email_missing")),
        crate::breadth::EmailOutcome::Failed(e) => Err(ApiError::Validation(json!({ "email": [format!("delivery_failed: {e}")] }))),
    }
}

/// `PUT /invoices/:id/sync_salesforce_id` — record the Salesforce id of an
/// invoice (`{ invoice: { external_id } }`) as the integration mapping on the
/// org's Salesforce connection.
pub async fn sync_salesforce_id(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let inv = st
        .db
        .find_invoice(id)
        .await?
        .filter(|i| i.organization_id == org)
        .ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let p = wrapped(&body, "invoice");
    let external_id = req_s(p, "external_id")?;
    let (connection_id, _) = st
        .db
        .find_connection_by_provider(org, "salesforce")
        .await?
        .ok_or_else(|| ApiError::validation("integration", "salesforce_not_connected"))?;
    st.db
        .insert_integration_mapping(Uuid::new_v4(), org, connection_id, "invoice", inv.id, &external_id)
        .await?;
    Ok(Json(json!({ "invoice": { "lago_id": inv.id, "external_salesforce_id": external_id } })))
}

/// `POST /invoices/:id/retry` — retry a `failed` invoice (tax/finalization
/// failure) by finalizing it again; any other state is returned unchanged.
pub async fn retry_invoice(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let inv = st
        .db
        .find_invoice(id)
        .await?
        .filter(|i| i.organization_id == auth.organization.id)
        .ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let inv = if matches!(inv.status, rago_core::enums::InvoiceStatus::Failed) {
        rago_jobs::finalize::finalize_invoice(&st.db, id)
            .await
            .map_err(|e| {
                tracing::error!(invoice_id = %id, error = %e, "invoice retry finalization failed");
                ApiError::Internal("invoice_finalization_failed".into())
            })?
            .unwrap_or(inv)
    } else {
        inv
    };
    Ok(Json(invoice_response(&st, inv).await?))
}

/// `POST /invoices/:id/retry_payment` — collect the invoice now through the
/// organization's payment provider (a retry of a failed charge uses a fresh
/// idempotency key; an already-paid invoice is never charged again).
pub async fn retry_payment(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let inv = st
        .db
        .find_invoice(id)
        .await?
        .filter(|i| i.organization_id == auth.organization.id)
        .ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let transport = rago_integrations::default_transport();
    let outcome = rago_jobs::collect_invoice(&st.db, &transport, &inv).await.map_err(|e| match e {
        rago_jobs::JobError::Db(db) => ApiError::from(db),
        other => ApiError::Validation(json!({ "payment": [other.to_string()] })),
    })?;
    let (retried, result) = match &outcome {
        rago_jobs::CollectOutcome::Charged(status) => (
            true,
            match status {
                rago_integrations::PayableStatus::Succeeded => "succeeded",
                rago_integrations::PayableStatus::Failed => "failed",
                rago_integrations::PayableStatus::Pending => "pending",
            },
        ),
        rago_jobs::CollectOutcome::Skipped(reason) => (false, reason.code()),
    };
    let refreshed = st.db.find_invoice(id).await?.unwrap_or(inv);
    Ok(Json(json!({
        "invoice": {
            "lago_id": refreshed.id,
            "payment_status": refreshed.payment_status.as_str(),
            "retried": retried,
            "result": result,
        }
    })))
}

/// `POST /invoices/:id/payment_url` — a one-time Stripe Checkout link to pay
/// the invoice.
pub async fn invoice_payment_url(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let inv = st
        .db
        .find_invoice(id)
        .await?
        .filter(|i| i.organization_id == auth.organization.id)
        .ok_or_else(|| ApiError::NotFound("invoice".into()))?;
    let transport = rago_integrations::default_transport();
    let url = match rago_jobs::collection::invoice_payment_url(&st.db, &transport, &inv).await {
        Ok(Ok(url)) => url,
        Ok(Err(code)) => return Err(ApiError::validation("payment_url", code)),
        Err(rago_jobs::JobError::Db(e)) => return Err(ApiError::from(e)),
        Err(e) => return Err(ApiError::Validation(json!({ "payment_url": [e.to_string()] }))),
    };
    Ok(Json(json!({ "invoice_payment_details": { "lago_invoice_id": inv.id, "payment_url": url } })))
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
