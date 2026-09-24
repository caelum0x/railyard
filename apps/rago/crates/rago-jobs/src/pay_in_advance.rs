//! Pay-in-advance charges — `Fees::CreatePayInAdvanceService` +
//! `Invoices::CreatePayInAdvanceChargeService`.
//!
//! A charge flagged `pay_in_advance` is billed the moment one of its events is
//! ingested instead of at period end: the fee is the *increment* the event adds
//! to the period's charge amount (price with the event minus price without it),
//! so graduated / package / percentage models bill correctly event by event.
//! The invoice is issued immediately and queued for collection; the period-end
//! run skips these charges (see `scheduler::load_charges`).

use chrono::Utc;
use rago_billing::invoices::assemble_invoice;
use rago_core::enums::{InvoiceStatus, InvoiceType, PaymentStatus};
use rago_core::models::{Event, Fee, Invoice, Subscription};
use rago_db::Db;
use rust_decimal::Decimal;
use serde_json::json;
use uuid::Uuid;

use crate::scheduler::{fee_webhook_object, invoice_webhook_object};
use crate::webhook_emit::{record_invoice_generated, record_webhook};
use crate::JobError;
use rago_integrations::webhooks::WebhookKind;

/// Job type: bill the pay-in-advance charges of one ingested event.
pub const JOB_PAY_IN_ADVANCE: &str = "events.pay_in_advance";

/// Whether the subscription's plan has a pay-in-advance charge on metric `code`
/// (the ingest path only enqueues the job when it does).
pub async fn has_pay_in_advance_charge(
    db: &Db,
    subscription: &Subscription,
    metric_code: &str,
) -> Result<bool, JobError> {
    for charge in db.charges_for_plan(subscription.plan_id).await? {
        if !charge.pay_in_advance {
            continue;
        }
        let Some(mid) = charge.billable_metric_id else { continue };
        if db.find_billable_metric(mid).await?.code == metric_code {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Enqueue the in-advance billing of `event` (no-op when it has no
/// pay-in-advance charge).
pub async fn enqueue_for_event(db: &Db, subscription: &Subscription, event: &Event) -> Result<bool, JobError> {
    if !has_pay_in_advance_charge(db, subscription, &event.code).await? {
        return Ok(false);
    }
    db.enqueue_job(
        JOB_PAY_IN_ADVANCE,
        &json!({ "event_id": event.id.to_string() }),
        Utc::now().naive_utc(),
        5,
    )
    .await?;
    Ok(true)
}

/// Bill the pay-in-advance charges of one event. Idempotent: a retry after the
/// invoice was written returns `Ok(None)`.
pub async fn bill_event_in_advance(db: &Db, event_id: Uuid) -> Result<Option<Invoice>, JobError> {
    let Some(event) = db.find_event_by_id(event_id).await? else {
        return Ok(None);
    };
    let Some(subscription_id) = event.subscription_id else {
        return Ok(None);
    };
    let subscription = db.find_subscription(subscription_id).await?;
    let plan = db.find_plan(subscription.plan_id).await?;
    let currency = plan.currency();

    let event_ts = event.timestamp.unwrap_or_else(|| event.created_at.naive_utc());
    // The usage period in progress (for pay-in-advance plans too: their
    // usage is billed at the end of the period it accrued in).
    let (from_date, to_date) = crate::scheduler::usage_period(&subscription, &plan, event_ts.date());
    let from = from_date.and_hms_opt(0, 0, 0).unwrap_or_default();
    let to = to_date.and_hms_opt(23, 59, 59).unwrap_or_default();

    let invoice_id = Uuid::new_v4();
    let mut fees: Vec<Fee> = Vec::new();
    let mut charge_ids: Vec<Uuid> = Vec::new();
    for charge in db.charges_for_plan(plan.id).await? {
        if !charge.pay_in_advance {
            continue;
        }
        let Some(mid) = charge.billable_metric_id else { continue };
        let metric = db.find_billable_metric(mid).await?;
        if metric.code != event.code {
            continue;
        }
        // Events of the period up to and including this one.
        let with: Vec<Event> = db
            .events_for_external_subscription(
                subscription.organization_id,
                &subscription.external_id,
                &metric.code,
                from,
                event_ts.min(to),
            )
            .await?;
        let without: Vec<Event> = with.iter().filter(|e| e.id != event.id).cloned().collect();
        let carry_in = db
            .weighted_sum_carry_in(subscription.organization_id, &subscription.external_id, &metric, from)
            .await?;
        let period = rago_billing::AggregationPeriod::new(from, to).with_initial_value(carry_in);

        let fee_with = rago_billing::charge_to_fee_in_period(
            &metric, &charge, &subscription, &with, currency, None, Some(&period),
        )?;
        let fee_without = rago_billing::charge_to_fee_in_period(
            &metric, &charge, &subscription, &without, currency, None, Some(&period),
        )?;
        let delta = fee_with.amount_cents - fee_without.amount_cents;
        charge_ids.push(charge.id);
        if delta <= 0 {
            continue;
        }
        let units = fee_with.units - fee_without.units;
        fees.push(Fee {
            id: Uuid::new_v4(),
            invoice_id: Some(invoice_id),
            amount_cents: delta,
            precise_amount_cents: Decimal::from(delta),
            units,
            events_count: Some(1),
            unit_amount_cents: if units.is_zero() {
                delta
            } else {
                (Decimal::from(delta) / units).round().try_into().unwrap_or(delta)
            },
            precise_unit_amount: if units.is_zero() {
                Decimal::from(delta)
            } else {
                Decimal::from(delta) / units
            },
            ..fee_with
        });
    }
    if charge_ids.is_empty() || fees.is_empty() {
        return Ok(None);
    }

    let mut invoice = in_advance_invoice(&subscription, invoice_id, &currency);
    let taxes = db.organization_taxes(subscription.organization_id).await?;
    assemble_invoice(&mut invoice, &mut fees, &taxes, &[]);
    invoice.payment_status = if invoice.total_amount_cents > 0 {
        PaymentStatus::Pending
    } else {
        PaymentStatus::Succeeded
    };
    if !db
        .insert_pay_in_advance_invoice(&mut invoice, &fees, event.id, &charge_ids)
        .await?
    {
        return Ok(None);
    }

    let org = subscription.organization_id;
    record_webhook(db, org, WebhookKind::InvoiceCreated, invoice_webhook_object(&invoice)).await?;
    record_invoice_generated(db, org, invoice.id, invoice_webhook_object(&invoice)).await?;
    for fee in &fees {
        record_webhook(db, org, WebhookKind::FeeCreated, fee_webhook_object(fee, &invoice)).await?;
    }
    if invoice.total_amount_cents > 0 {
        db.enqueue_job(
            crate::queue::JOB_COLLECT_INVOICE,
            &json!({ "invoice_id": invoice.id.to_string() }),
            Utc::now().naive_utc(),
            5,
        )
        .await?;
    }
    Ok(Some(invoice))
}

fn in_advance_invoice(
    subscription: &Subscription,
    id: Uuid,
    currency: &rago_core::money::Currency,
) -> Invoice {
    let now = Utc::now();
    Invoice {
        id,
        organization_id: subscription.organization_id,
        billing_entity_id: subscription
            .billing_entity_id
            .unwrap_or(subscription.organization_id),
        customer_id: Some(subscription.customer_id),
        number: String::new(),
        sequential_id: None,
        invoice_type: InvoiceType::Subscription,
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
    }
}
