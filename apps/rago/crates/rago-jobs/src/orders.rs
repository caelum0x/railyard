//! Quote → order form → order → invoice.
//!
//! * `OrderForms::CreateService` — an approved quote version is issued as an
//!   order form for the customer to sign (`order_form.created`).
//! * `OrderForms::MarkAsSignedService` — signing creates the order
//!   (`order.created`).
//! * `Orders::ExecuteService` — executing the order bills the quote's line
//!   items on a one-off invoice (add-on fees, organization taxes), accepts the
//!   quote, queues the invoice for collection (`order.executed`).

use chrono::Utc;
use rago_billing::invoices::assemble_invoice;
use rago_core::enums::{FeeType, InvoiceStatus, InvoiceType, PaymentStatus};
use rago_core::models::{Fee, Invoice};
use rago_core::money::Currency;
use rago_db::{Db, OrderFormRow, OrderRow};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::scheduler::invoice_webhook_object;
use crate::webhook_emit::{record_invoice_generated, record_webhook};
use crate::JobError;
use rago_integrations::webhooks::WebhookKind;

/// Why an order-flow step was refused (a caller-facing validation code).
#[derive(Debug, thiserror::Error)]
pub enum OrderFlowError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error(transparent)]
    Job(#[from] JobError),
}

impl From<rago_db::DbError> for OrderFlowError {
    fn from(e: rago_db::DbError) -> Self {
        OrderFlowError::Job(JobError::Db(e))
    }
}

fn document_number(prefix: &str) -> String {
    format!("{prefix}-{}", &Uuid::new_v4().simple().to_string()[..8].to_uppercase())
}

/// Issue an order form from the quote's approved version. Idempotent: an
/// existing non-voided form for the quote is returned as is.
pub async fn create_order_form(db: &Db, org: Uuid, quote_id: Uuid) -> Result<OrderFormRow, OrderFlowError> {
    let version = db
        .approved_quote_version(quote_id)
        .await?
        .filter(|v| v.organization_id == org)
        .ok_or(OrderFlowError::Invalid("quote_has_no_approved_version"))?;
    if let Some(existing) = db.open_order_form_for_quote(quote_id).await? {
        return Ok(existing);
    }
    let form = db
        .insert_order_form(Uuid::new_v4(), org, Some(version.customer_id), Some(quote_id), &document_number("OF"))
        .await?;
    // The form is signable only while the approved offer stands; the
    // ExpireOrderForms clock expires it afterwards (`order_form.expired`).
    if version.expires_at.is_some() {
        db.set_order_form_expires_at(form.0, version.expires_at).await?;
    }
    record_webhook(
        db,
        org,
        WebhookKind::OrderFormCreated,
        json!({
            "lago_id": form.0,
            "number": form.2,
            "status": form.3,
            "lago_customer_id": version.customer_id,
            "lago_quote_id": quote_id,
            "lago_quote_version_id": version.version_id,
            "currency": version.currency,
            "total_amount_cents": version.total_amount_cents,
            "expires_at": version.expires_at.map(|t| t.and_utc().to_rfc3339()),
        }),
    )
    .await?;
    Ok(form)
}

/// After an order form is signed: create its order (once).
pub async fn create_order_for_signed_form(
    db: &Db,
    org: Uuid,
    order_form_id: Uuid,
) -> Result<OrderRow, OrderFlowError> {
    let form = db
        .find_order_form(order_form_id)
        .await?
        .ok_or(OrderFlowError::Invalid("order_form_not_found"))?;
    let (form_org, _) = db
        .order_form_origin(order_form_id)
        .await?
        .ok_or(OrderFlowError::Invalid("order_form_not_found"))?;
    if form_org != org {
        return Err(OrderFlowError::Invalid("order_form_not_found"));
    }
    if form.3 != "signed" {
        return Err(OrderFlowError::Invalid("order_form_not_signed"));
    }
    if let Some(existing) = db.order_for_form(order_form_id).await? {
        return Ok(existing);
    }
    let order = db
        .insert_order(Uuid::new_v4(), org, form.1, Some(order_form_id), &document_number("ORD"))
        .await?;
    record_webhook(
        db,
        org,
        WebhookKind::OrderCreated,
        json!({
            "lago_id": order.0,
            "number": order.2,
            "status": order.3,
            "lago_customer_id": order.1,
            "lago_order_form_id": order_form_id,
        }),
    )
    .await?;
    Ok(order)
}

/// One billable line of a quote.
struct Line {
    name: String,
    add_on_id: Option<Uuid>,
    unit_amount_cents: i64,
    units: Decimal,
}

fn dec(v: Option<&Value>) -> Option<Decimal> {
    match v? {
        Value::String(s) => s.trim().parse().ok(),
        Value::Number(n) => n.to_string().parse().ok(),
        _ => None,
    }
}

async fn quote_lines(db: &Db, org: Uuid, items: &Value) -> Result<Vec<Line>, OrderFlowError> {
    let arr = items.as_array().ok_or(OrderFlowError::Invalid("quote_line_items_invalid"))?;
    let mut lines = Vec::with_capacity(arr.len());
    for it in arr {
        let units = dec(it.get("units")).unwrap_or(Decimal::ONE);
        if units <= Decimal::ZERO {
            return Err(OrderFlowError::Invalid("quote_line_units_invalid"));
        }
        let explicit_unit = it.get("unit_amount_cents").and_then(Value::as_i64);
        if let Some(code) = it.get("add_on_code").and_then(Value::as_str) {
            let (id, name, _, amount_cents, _) = db
                .find_add_on_by_code(org, code)
                .await?
                .ok_or(OrderFlowError::Invalid("quote_add_on_not_found"))?;
            lines.push(Line { name, add_on_id: Some(id), unit_amount_cents: explicit_unit.unwrap_or(amount_cents), units });
        } else {
            let name = it
                .get("name")
                .and_then(Value::as_str)
                .ok_or(OrderFlowError::Invalid("quote_line_name_missing"))?
                .to_string();
            let unit = explicit_unit.ok_or(OrderFlowError::Invalid("quote_line_amount_missing"))?;
            lines.push(Line { name, add_on_id: None, unit_amount_cents: unit, units });
        }
    }
    if lines.is_empty() {
        return Err(OrderFlowError::Invalid("quote_has_no_line_items"));
    }
    Ok(lines)
}

/// Execute a pending order: invoice the quote's line items. Exactly once.
pub async fn execute_order(db: &Db, org: Uuid, order_id: Uuid) -> Result<(OrderRow, Invoice), OrderFlowError> {
    let ctx = db
        .order_context(order_id)
        .await?
        .filter(|c| c.organization_id == org)
        .ok_or(OrderFlowError::Invalid("order_not_found"))?;
    if ctx.status != "pending" {
        return Err(OrderFlowError::Invalid("order_not_pending"));
    }
    let quote_id = ctx.quote_id.ok_or(OrderFlowError::Invalid("order_has_no_quote"))?;
    // A signed order commits to the approved version even if the offer has
    // since passed its expiry date.
    let version = db
        .committed_quote_version(quote_id)
        .await?
        .ok_or(OrderFlowError::Invalid("quote_has_no_approved_version"))?;
    let customer_id = ctx.customer_id.unwrap_or(version.customer_id);
    let lines = quote_lines(db, org, &version.line_items).await?;

    if !db.claim_order_execution(order_id).await? {
        return Err(OrderFlowError::Invalid("order_not_pending"));
    }
    let invoice = match issue_order_invoice(db, org, customer_id, &version.currency, &lines).await {
        Ok(inv) => inv,
        Err(e) => {
            db.release_order_execution(order_id).await?;
            return Err(e);
        }
    };
    db.accept_quote(quote_id, invoice.id).await?;
    let order = db
        .find_order(order_id)
        .await?
        .ok_or(OrderFlowError::Invalid("order_not_found"))?;

    record_webhook(
        db,
        org,
        WebhookKind::OrderExecuted,
        json!({
            "lago_id": order.0,
            "number": order.2,
            "status": order.3,
            "lago_customer_id": customer_id,
            "lago_invoice_id": invoice.id,
        }),
    )
    .await?;
    record_webhook(db, org, WebhookKind::InvoiceCreated, invoice_webhook_object(&invoice)).await?;
    record_invoice_generated(db, org, invoice.id, invoice_webhook_object(&invoice)).await?;
    if invoice.total_amount_cents > 0 {
        db.enqueue_job(
            crate::queue::JOB_COLLECT_INVOICE,
            &json!({ "invoice_id": invoice.id.to_string() }),
            Utc::now().naive_utc(),
            5,
        )
        .await?;
    }
    Ok((order, invoice))
}

async fn issue_order_invoice(
    db: &Db,
    org: Uuid,
    customer_id: Uuid,
    currency_code: &str,
    lines: &[Line],
) -> Result<Invoice, OrderFlowError> {
    let currency = Currency::new(currency_code);
    let now = Utc::now();
    let invoice_id = Uuid::new_v4();
    let mut fees: Vec<Fee> = lines
        .iter()
        .map(|l| {
            let amount_cents: i64 = (Decimal::from(l.unit_amount_cents) * l.units)
                .round()
                .try_into()
                .unwrap_or(0);
            Fee {
                id: Uuid::new_v4(),
                organization_id: org,
                billing_entity_id: org,
                invoice_id: Some(invoice_id),
                subscription_id: None,
                charge_id: None,
                charge_filter_id: None,
                fee_type: Some(FeeType::AddOn),
                invoiceable_type: Some("AddOn".into()),
                invoiceable_id: l.add_on_id,
                amount_cents,
                amount_currency: currency.code(),
                precise_amount_cents: Decimal::from(amount_cents),
                unit_amount_cents: l.unit_amount_cents,
                precise_unit_amount: Decimal::from(l.unit_amount_cents) / currency.subunit_to_unit_decimal(),
                taxes_amount_cents: 0,
                taxes_precise_amount_cents: Decimal::ZERO,
                taxes_rate: 0.0,
                units: l.units,
                total_aggregated_units: None,
                events_count: None,
                precise_coupons_amount_cents: Decimal::ZERO,
                precise_credit_notes_amount_cents: Decimal::ZERO,
                payment_status: PaymentStatus::Pending,
                properties: json!({ "name": l.name }),
                amount_details: json!({}),
                grouped_by: json!({}),
                created_at: now,
                updated_at: now,
                applied_taxes: Vec::new(),
            }
        })
        .collect();
    let mut invoice = Invoice {
        id: invoice_id,
        organization_id: org,
        billing_entity_id: org,
        customer_id: Some(customer_id),
        number: String::new(),
        sequential_id: None,
        invoice_type: InvoiceType::OneOff,
        status: InvoiceStatus::Finalized,
        payment_status: PaymentStatus::Pending,
        currency: Some(currency.code()),
        issuing_date: Some(now.date_naive()),
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
        created_at: now,
        updated_at: now,
        applied_taxes: Vec::new(),
    };
    let taxes = db.organization_taxes(org).await?;
    assemble_invoice(&mut invoice, &mut fees, &taxes, &[]);
    invoice.payment_status = if invoice.total_amount_cents > 0 {
        PaymentStatus::Pending
    } else {
        PaymentStatus::Succeeded
    };
    db.insert_numbered_invoice_with_fees(&mut invoice, &fees).await?;
    Ok(invoice)
}
