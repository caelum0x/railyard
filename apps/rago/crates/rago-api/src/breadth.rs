//! Phase-13 breadth handlers — add-ons, one-off invoices, quotes, data exports,
//! wallet recurring/threshold top-up rules, and filtered index endpoints.
//!
//! One-off invoices and quotes both reuse the same fee/tax engine
//! (`rago_billing::invoices::assemble_invoice`) as subscription billing, so the
//! money math is identical across every path.

use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{NaiveDate, Utc};
use rago_billing::invoices::assemble_invoice;
use rago_core::enums::*;
use rago_core::models::*;
use rago_core::money::Currency;
use rago_mail::builders;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult};
use crate::handlers::{dec_of, i64_of, now, req_s, s, wrapped};
use crate::pagination::{self, PageParams};
use crate::serializers as ser;
use crate::state::AppState;

/// Must match `rago_jobs::data_export::JOB_DATA_EXPORT` (producer/consumer are
/// decoupled by the durable queue, so the API does not depend on the worker).
const JOB_DATA_EXPORT: &str = "data_export";

// ---- add-ons ----------------------------------------------------------------

/// `POST /add_ons` — create a reusable one-off charge template.
pub async fn create_add_on(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let p = wrapped(&body, "add_on");
    let id = Uuid::new_v4();
    let name = req_s(p, "name")?;
    let code = req_s(p, "code")?;
    let amount_cents = i64_of(p, "amount_cents").ok_or_else(|| ApiError::validation("amount_cents", "value_is_mandatory"))?;
    let currency = s(p, "amount_currency").unwrap_or_else(|| "USD".into());
    st.db.insert_add_on(id, auth.organization.id, &name, &code, s(p, "description").as_deref(), amount_cents, &currency).await?;
    Ok(Json(json!({
        "add_on": {
            "lago_id": id, "name": name, "code": code,
            "amount_cents": amount_cents, "amount_currency": currency,
        }
    })))
}

// ---- one-off invoices -------------------------------------------------------

/// A parsed ad-hoc fee line for a one-off invoice or quote line item.
struct AdHocLine {
    name: String,
    unit_amount_cents: i64,
    units: Decimal,
}

/// Resolve request fee/line-item entries into ad-hoc lines, resolving
/// `add_on_code` against the `add_ons` table.
async fn resolve_lines(st: &AppState, org: Uuid, arr: &[Value]) -> ApiResult<Vec<AdHocLine>> {
    let mut lines = Vec::new();
    for it in arr {
        let units = dec_of(it, "units").unwrap_or(Decimal::ONE);
        if let Some(code) = s(it, "add_on_code") {
            let (_, name, _, amount_cents, _) = st
                .db
                .find_add_on_by_code(org, &code)
                .await?
                .ok_or_else(|| ApiError::NotFound("add_on".into()))?;
            let unit = i64_of(it, "unit_amount_cents").unwrap_or(amount_cents);
            lines.push(AdHocLine { name, unit_amount_cents: unit, units });
        } else {
            let name = req_s(it, "name")?;
            let unit = i64_of(it, "unit_amount_cents")
                .ok_or_else(|| ApiError::validation("unit_amount_cents", "value_is_mandatory"))?;
            lines.push(AdHocLine { name, unit_amount_cents: unit, units });
        }
    }
    if lines.is_empty() {
        return Err(ApiError::validation("fees", "must_be_an_array"));
    }
    Ok(lines)
}

/// Build the ad-hoc fees for an org/customer from parsed lines (add-on fee type).
fn build_adhoc_fees(org: Uuid, currency: &Currency, lines: &[AdHocLine]) -> Vec<Fee> {
    lines
        .iter()
        .map(|l| {
            let amount_cents = (Decimal::from(l.unit_amount_cents) * l.units)
                .round()
                .try_into()
                .unwrap_or(0i64);
            Fee {
                id: Uuid::new_v4(),
                organization_id: org,
                billing_entity_id: org,
                invoice_id: None,
                subscription_id: None,
                charge_id: None,
                charge_filter_id: None,
                fee_type: Some(FeeType::AddOn),
                invoiceable_type: Some("AddOn".to_string()),
                invoiceable_id: None,
                amount_cents,
                amount_currency: currency.code(),
                precise_amount_cents: Decimal::from(amount_cents),
                unit_amount_cents: l.unit_amount_cents,
                precise_unit_amount: Decimal::from(l.unit_amount_cents) / currency.subunit_to_unit_decimal(),
                taxes_amount_cents: 0,
                taxes_precise_amount_cents: Decimal::ZERO,
                taxes_rate: 0.0,
                units: l.units,
                total_aggregated_units: Some(l.units),
                events_count: None,
                precise_coupons_amount_cents: Decimal::ZERO,
                precise_credit_notes_amount_cents: Decimal::ZERO,
                payment_status: PaymentStatus::Pending,
                properties: json!({ "name": l.name }),
                amount_details: json!({}),
                grouped_by: json!({}),
                created_at: now(),
                updated_at: now(),
                applied_taxes: Vec::new(),
            }
        })
        .collect()
}

fn adhoc_invoice(org: Uuid, customer_id: Uuid, currency: &Currency, itype: InvoiceType) -> Invoice {
    Invoice {
        id: Uuid::new_v4(),
        organization_id: org,
        billing_entity_id: org,
        customer_id: Some(customer_id),
        number: format!("INV-{}", &Uuid::new_v4().to_string()[..8]),
        sequential_id: Some(1),
        invoice_type: itype,
        status: InvoiceStatus::Finalized,
        payment_status: PaymentStatus::Pending,
        currency: Some(currency.code()),
        issuing_date: Some(now().date_naive()),
        fees_amount_cents: 0,
        coupons_amount_cents: 0,
        credit_notes_amount_cents: 0,
        prepaid_credit_amount_cents: 0,
        progressive_billing_credit_amount_cents: 0,
        sub_total_excluding_taxes_amount_cents: 0,
        sub_total_including_taxes_amount_cents: 0,
        taxes_amount_cents: 0,
        taxes_rate: 0.0,
        total_amount_cents: 0,
        version_number: 4,
        created_at: now(),
        updated_at: now(),
        applied_taxes: Vec::new(),
    }
}

/// `POST /invoices` — create an ad-hoc (one-off) invoice from add-on / arbitrary
/// fees, reusing the fee/tax engine (`Invoices::CreateService`).
pub async fn create_one_off_invoice(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let p = wrapped(&body, "invoice");
    let external_customer_id = req_s(p, "external_customer_id")?;
    let customer = st
        .db
        .find_customer_by_external_id(org, &external_customer_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("customer".into()))?;
    let currency = Currency::new(&s(p, "currency").or(customer.currency.clone()).unwrap_or_else(|| "USD".into()));

    let arr = p.get("fees").and_then(Value::as_array).cloned().unwrap_or_default();
    let lines = resolve_lines(&st, org, &arr).await?;
    let mut fees = build_adhoc_fees(org, &currency, &lines);
    let mut invoice = adhoc_invoice(org, customer.id, &currency, InvoiceType::OneOff);
    for fee in &mut fees {
        fee.invoice_id = Some(invoice.id);
    }

    let taxes = st.db.organization_taxes(org).await?;
    assemble_invoice(&mut invoice, &mut fees, &taxes, &[]);

    st.db.insert_invoice(&invoice).await?;
    for fee in &fees {
        st.db.insert_fee(fee).await?;
    }
    let _ = st.db.insert_activity_log(
        org, "invoice.one_off_created", "Invoice", invoice.id,
        Some(&external_customer_id), None,
        &json!({ "number": invoice.number, "total_amount_cents": invoice.total_amount_cents }),
    ).await;

    crate::emit::webhook(
        &st.db,
        org,
        rago_integrations::webhooks::WebhookKind::InvoiceOneOffCreated,
        ser::invoice(&invoice, &fees),
    )
    .await;
    if matches!(invoice.status, rago_core::enums::InvoiceStatus::Finalized) {
        let _ = rago_jobs::webhook_emit::record_invoice_generated(&st.db, org, invoice.id, ser::invoice(&invoice, &fees)).await;
    }
    send_invoice_finalized(&st, &invoice, &fees, customer.email.as_deref()).await;

    Ok(Json(json!({ "invoice": ser::invoice(&invoice, &fees) })))
}

/// Render the invoice-finalized email and send it through the injectable
/// transport (`InvoiceMailer#finalized`). Billing paths treat it as
/// best-effort (a mail failure never fails the billing request); the result
/// says what actually happened so an explicit resend can report it.
pub async fn send_invoice_finalized(st: &AppState, invoice: &Invoice, fees: &[Fee], to: Option<&str>) -> EmailOutcome {
    let Some(to) = to.filter(|t| t.contains('@')) else {
        tracing::info!(invoice_id = %invoice.id, "invoice email skipped: customer has no email address");
        return EmailOutcome::NoRecipient;
    };
    let email = builders::invoice_finalized_email(invoice, fees, to);
    match st.mailer.send(&email).await {
        Ok(_) => {
            crate::observability::incr("emails_sent");
            EmailOutcome::Sent
        }
        Err(e) => {
            tracing::warn!(invoice_id = %invoice.id, error = %e, "invoice email failed");
            EmailOutcome::Failed(e.to_string())
        }
    }
}

/// What happened to an invoice email.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmailOutcome {
    Sent,
    NoRecipient,
    Failed(String),
}

/// `GET /emails` — captured emails (verification only; only populated when the
/// capture transport is active, i.e. non-`live` builds). Not part of Lago's
/// surface. The capture buffer is process-wide (not tenant-scoped), so the
/// route is disabled unless the operator opts in with
/// `RAGO_EXPOSE_CAPTURED_EMAILS=true` on a single-tenant/dev deployment.
pub async fn list_captured_emails(State(st): State<AppState>, _auth: Authed) -> ApiResult<Json<Value>> {
    let enabled = std::env::var("RAGO_EXPOSE_CAPTURED_EMAILS")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    if !enabled {
        return Err(ApiError::NotFound("email".into()));
    }
    let sent = st.capture.as_ref().map(|c| c.sent()).unwrap_or_default();
    let emails: Vec<Value> = sent.iter().map(|e| json!({
        "to": e.to, "from": e.from, "subject": e.subject,
        "text_body": e.text_body, "html_body": e.html_body,
    })).collect();
    Ok(Json(json!({ "emails": emails, "count": sent.len() })))
}

// ---- quotes -----------------------------------------------------------------

/// `POST /quotes` — generate a quote for a customer from line items, computing
/// totals with the same fee/tax engine (but persisting nothing billable).
pub async fn create_quote(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let p = wrapped(&body, "quote");
    let external_customer_id = req_s(p, "external_customer_id")?;
    let customer = st
        .db
        .find_customer_by_external_id(org, &external_customer_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("customer".into()))?;
    let currency = Currency::new(&s(p, "currency").or(customer.currency.clone()).unwrap_or_else(|| "USD".into()));

    let arr = p.get("line_items").and_then(Value::as_array).cloned().unwrap_or_default();
    let lines = resolve_lines(&st, org, &arr).await?;
    let mut fees = build_adhoc_fees(org, &currency, &lines);
    let mut draft = adhoc_invoice(org, customer.id, &currency, InvoiceType::OneOff);
    let taxes = st.db.organization_taxes(org).await?;
    assemble_invoice(&mut draft, &mut fees, &taxes, &[]);

    // Persist the quote with the resolved line items (for later acceptance).
    let id = Uuid::new_v4();
    let number = format!("QUO-{}", &id.to_string()[..8]);
    let line_items = json!(lines
        .iter()
        .map(|l| json!({ "name": l.name, "unit_amount_cents": l.unit_amount_cents, "units": l.units.to_string() }))
        .collect::<Vec<_>>());
    st.db.insert_quote(
        id, org, customer.id, &number, &currency.code(),
        draft.sub_total_excluding_taxes_amount_cents, draft.taxes_amount_cents, draft.total_amount_cents,
        &line_items,
    ).await?;

    Ok(Json(json!({
        "quote": {
            "lago_id": id, "number": number, "status": "draft",
            "external_customer_id": external_customer_id,
            "currency": currency.code(),
            "sub_total_excluding_taxes_amount_cents": draft.sub_total_excluding_taxes_amount_cents,
            "taxes_amount_cents": draft.taxes_amount_cents,
            "total_amount_cents": draft.total_amount_cents,
            "line_items": line_items,
        }
    })))
}

/// `POST /quotes/:id/accept` — accept a quote: materialize its line items into a
/// one-off invoice (same engine, same totals) and mark the quote accepted.
pub async fn accept_quote(
    State(st): State<AppState>,
    auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let (quote_id, customer_id, status, currency_code, _total, line_items) = st
        .db
        .find_quote(id)
        .await?
        .ok_or_else(|| ApiError::NotFound("quote".into()))?;
    if status == "accepted" {
        return Err(ApiError::validation("quote", "already_accepted"));
    }
    let currency = Currency::new(&currency_code);
    let arr = line_items.as_array().cloned().unwrap_or_default();
    let lines = resolve_lines(&st, org, &arr).await?;
    let mut fees = build_adhoc_fees(org, &currency, &lines);
    let mut invoice = adhoc_invoice(org, customer_id, &currency, InvoiceType::OneOff);
    for fee in &mut fees {
        fee.invoice_id = Some(invoice.id);
    }
    let taxes = st.db.organization_taxes(org).await?;
    assemble_invoice(&mut invoice, &mut fees, &taxes, &[]);

    st.db.insert_invoice(&invoice).await?;
    for fee in &fees {
        st.db.insert_fee(fee).await?;
    }
    st.db.accept_quote(quote_id, invoice.id).await?;

    let email = st.db.customer_external_id(customer_id).await?;
    let _ = email;
    send_invoice_finalized(&st, &invoice, &fees, None).await;

    Ok(Json(json!({
        "quote": { "lago_id": quote_id, "status": "accepted", "accepted_invoice_id": invoice.id },
        "invoice": ser::invoice(&invoice, &fees),
    })))
}

// ---- wallet recurring / threshold top-up rules ------------------------------

/// `POST /wallets/:id/recurring_rules` — attach an interval or threshold
/// auto-top-up rule to a wallet (`RecurringTransactionRules::*`).
pub async fn create_wallet_recurring_rule(
    State(st): State<AppState>,
    auth: Authed,
    Path(wallet_id): Path<Uuid>,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let wallet = st.db.find_wallet(wallet_id).await?.ok_or_else(|| ApiError::NotFound("wallet".into()))?;
    let p = wrapped(&body, "recurring_rule");
    let rule_type = s(p, "rule_type").unwrap_or_else(|| "interval".into());
    if rule_type != "interval" && rule_type != "threshold" {
        return Err(ApiError::validation("rule_type", "value_is_invalid"));
    }
    let interval = s(p, "interval");
    let threshold = dec_of(p, "threshold_credits");
    if rule_type == "interval" && interval.is_none() {
        return Err(ApiError::validation("interval", "value_is_mandatory"));
    }
    if rule_type == "threshold" && threshold.is_none() {
        return Err(ApiError::validation("threshold_credits", "value_is_mandatory"));
    }
    let paid = dec_of(p, "paid_credits").unwrap_or(Decimal::ZERO);
    let granted = dec_of(p, "granted_credits").unwrap_or(Decimal::ZERO);
    let id = Uuid::new_v4();
    st.db.insert_wallet_recurring_rule(id, org, wallet.id, &rule_type, interval.as_deref(), threshold, paid, granted).await?;

    Ok(Json(json!({
        "recurring_rule": {
            "lago_id": id, "lago_wallet_id": wallet.id, "rule_type": rule_type,
            "interval": interval, "threshold_credits": threshold.map(|d| d.to_string()),
            "paid_credits": paid.to_string(), "granted_credits": granted.to_string(),
        }
    })))
}

// ---- data exports -----------------------------------------------------------

/// `POST /data_exports` — enqueue an async CSV export (the worker builds it).
pub async fn create_data_export(
    State(st): State<AppState>,
    auth: Authed,
    Json(body): Json<Value>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let p = wrapped(&body, "data_export");
    let resource_type = req_s(p, "resource_type")?;
    if !matches!(resource_type.as_str(), "invoices" | "events" | "customers" | "credit_notes") {
        return Err(ApiError::validation("resource_type", "value_is_invalid"));
    }
    let id = Uuid::new_v4();
    st.db.insert_data_export(id, org, &resource_type).await?;
    let payload = json!({
        "data_export_id": id.to_string(),
        "organization_id": org.to_string(),
        "resource_type": resource_type,
    });
    st.db.enqueue_job(JOB_DATA_EXPORT, &payload, Utc::now().naive_utc(), 5).await?;

    Ok(Json(json!({
        "data_export": { "lago_id": id, "resource_type": resource_type, "format": "csv", "status": "pending" }
    })))
}

/// `GET /data_exports/:id` — export status + row count.
pub async fn show_data_export(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Value>> {
    let (resource_type, status, _content, row_count) =
        st.db.find_data_export(id).await?.ok_or_else(|| ApiError::NotFound("data_export".into()))?;
    Ok(Json(json!({
        "data_export": { "lago_id": id, "resource_type": resource_type, "format": "csv", "status": status, "row_count": row_count }
    })))
}

/// `GET /data_exports/:id/download` — the finished CSV.
pub async fn download_data_export(
    State(st): State<AppState>,
    _auth: Authed,
    Path(id): Path<Uuid>,
) -> Result<axum::response::Response, ApiError> {
    use axum::response::IntoResponse;
    let (resource_type, status, content, _rows) =
        st.db.find_data_export(id).await?.ok_or_else(|| ApiError::NotFound("data_export".into()))?;
    if status != "completed" {
        return Err(ApiError::validation("status", "export_not_ready"));
    }
    let csv = content.unwrap_or_default();
    let headers = [
        (axum::http::header::CONTENT_TYPE, "text/csv".to_string()),
        (
            axum::http::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{resource_type}-{id}.csv\""),
        ),
    ];
    Ok((headers, csv).into_response())
}

// ---- filtered index endpoints -----------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SubscriptionFilters {
    #[serde(flatten)]
    pub page: PageParams,
    pub status: Option<String>,
    pub external_id: Option<String>,
}

/// `GET /subscriptions` — filter by `status` and `external_id` substring.
pub async fn list_subscriptions(
    State(st): State<AppState>,
    auth: Authed,
    Query(f): Query<SubscriptionFilters>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let status = f.status.as_deref().and_then(subscription_status_i32);
    let subs = st.db.list_subscriptions(org, status, f.external_id.as_deref(), f.page.per_page(), f.page.offset()).await?;
    let total = st.db.count_subscriptions(org, status, f.external_id.as_deref()).await?;
    let mut out = Vec::new();
    for sub in &subs {
        let plan = st.db.find_plan(sub.plan_id).await?;
        let ext = st.db.customer_external_id(sub.customer_id).await?.unwrap_or_default();
        out.push(ser::subscription(sub, &plan, &ext));
    }
    Ok(Json(json!({ "subscriptions": out, "meta": pagination::meta(f.page.page(), f.page.per_page(), total) })))
}

#[derive(Debug, Deserialize)]
pub struct InvoiceFilters {
    #[serde(flatten)]
    pub page: PageParams,
    pub status: Option<String>,
    pub payment_status: Option<String>,
    pub issuing_date_from: Option<String>,
    pub issuing_date_to: Option<String>,
}

/// `GET /invoices` — filter by `status`, `payment_status`, issuing-date range.
pub async fn list_invoices(
    State(st): State<AppState>,
    auth: Authed,
    Query(f): Query<InvoiceFilters>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let status = f.status.as_deref().and_then(invoice_status_i32);
    let payment_status = f.payment_status.as_deref().and_then(payment_status_i32);
    let from = f.issuing_date_from.as_deref().and_then(parse_date);
    let to = f.issuing_date_to.as_deref().and_then(parse_date);
    let invoices = st.db.list_invoices(org, status, payment_status, from, to, f.page.per_page(), f.page.offset()).await?;
    let total = st.db.count_invoices(org, status, payment_status, from, to).await?;
    let out: Vec<Value> = invoices.iter().map(|i| ser::invoice(i, &[])).collect();
    Ok(Json(json!({ "invoices": out, "meta": pagination::meta(f.page.page(), f.page.per_page(), total) })))
}

#[derive(Debug, Deserialize)]
pub struct EventFilters {
    #[serde(flatten)]
    pub page: PageParams,
    pub code: Option<String>,
    pub external_subscription_id: Option<String>,
}

/// `GET /events` — filter by `code` and `external_subscription_id`.
pub async fn list_events(
    State(st): State<AppState>,
    auth: Authed,
    Query(f): Query<EventFilters>,
) -> ApiResult<Json<Value>> {
    let org = auth.organization.id;
    let events = st.db.list_events(org, f.code.as_deref(), f.external_subscription_id.as_deref(), f.page.per_page(), f.page.offset()).await?;
    let total = st.db.count_events(org, f.code.as_deref(), f.external_subscription_id.as_deref()).await?;
    let out: Vec<Value> = events.iter().map(ser::event).collect();
    Ok(Json(json!({ "events": out, "meta": pagination::meta(f.page.page(), f.page.per_page(), total) })))
}

// ---- enum string → int helpers ---------------------------------------------

fn subscription_status_i32(s: &str) -> Option<i32> {
    Some(match s {
        "pending" => SubscriptionStatus::Pending,
        "active" => SubscriptionStatus::Active,
        "terminated" => SubscriptionStatus::Terminated,
        "canceled" => SubscriptionStatus::Canceled,
        _ => return None,
    }.as_i32())
}
fn invoice_status_i32(s: &str) -> Option<i32> {
    Some(match s {
        "draft" => InvoiceStatus::Draft,
        "finalized" => InvoiceStatus::Finalized,
        "voided" => InvoiceStatus::Voided,
        "generating" => InvoiceStatus::Generating,
        "failed" => InvoiceStatus::Failed,
        "open" => InvoiceStatus::Open,
        "closed" => InvoiceStatus::Closed,
        "pending" => InvoiceStatus::Pending,
        _ => return None,
    }.as_i32())
}
fn payment_status_i32(s: &str) -> Option<i32> {
    Some(match s {
        "pending" => PaymentStatus::Pending,
        "succeeded" => PaymentStatus::Succeeded,
        "failed" => PaymentStatus::Failed,
        "refunded" => PaymentStatus::Refunded,
        _ => return None,
    }.as_i32())
}
fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}
