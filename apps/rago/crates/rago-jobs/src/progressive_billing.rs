//! Progressive billing — `LifetimeUsages::RecalculateAndCheckJob` +
//! `Invoices::ProgressiveBillingService`.
//!
//! On every lifetime-usage refresh, each subscription with usage thresholds in
//! force (its own, or inherited from its plan) is checked: the current period's
//! usage is priced, and when it crosses a threshold a `progressive_billing`
//! invoice is issued for the usage not yet billed this period. The period-end
//! invoice later credits that usage back (see `scheduler::bill_one`), and any
//! over-billing becomes a true-up credit note.

use std::collections::HashMap;

use chrono::{Duration, NaiveDate, Utc};
use rago_billing::dates::{due_for_billing, Interval};
use rago_billing::invoices::assemble_invoice;
use rago_billing::progressive_billing::ProgressiveCreditNote;
use rago_billing::usage_thresholds::{check_thresholds, LifetimeUsage, UsageThreshold};
use rago_core::enums::{InvoiceStatus, InvoiceType, PaymentStatus};
use rago_core::models::{CreditNote, Fee, Invoice, Subscription};
use rago_core::money::Currency;
use rago_db::Db;
use rust_decimal::Decimal;
use serde_json::json;
use uuid::Uuid;

use crate::scheduler::{fee_webhook_object, invoice_webhook_object, load_charges};
use crate::webhook_emit::{record_credit_note_generated, record_invoice_generated, record_webhook};
use crate::JobError;
use rago_integrations::webhooks::WebhookKind;

/// Longest horizon searched for the next billing date (covers yearly plans).
const MAX_PERIOD_DAYS: i64 = 400;

/// The next date after `today` on which the subscription's period rolls over.
pub fn next_billing_date(
    interval: Interval,
    anniversary: bool,
    subscription_at: NaiveDate,
    today: NaiveDate,
) -> NaiveDate {
    (1..=MAX_PERIOD_DAYS)
        .map(|d| today + Duration::days(d))
        .find(|d| due_for_billing(interval, anniversary, subscription_at, *d))
        .unwrap_or(today + Duration::days(1))
}

/// Check every subscription that has thresholds in force. Returns the number
/// of progressive invoices issued. One subscription failing is logged and does
/// not stop the sweep.
pub async fn run_usage_threshold_checks(db: &Db) -> Result<u64, JobError> {
    let today = Utc::now().date_naive();
    let mut issued = 0;
    for subscription_id in db.subscriptions_with_usage_thresholds().await? {
        let subscription = db.find_subscription(subscription_id).await?;
        match check_usage_thresholds(db, &subscription, today).await {
            Ok(Some(_)) => issued += 1,
            Ok(None) => {}
            Err(e) => tracing::error!(
                subscription_id = %subscription_id,
                error = %e,
                "usage-threshold check failed; continuing"
            ),
        }
    }
    Ok(issued)
}

/// Check one subscription's usage against its thresholds and, when any was
/// crossed, issue the progressive invoice (queued for collection) and emit
/// `invoice.created` + `subscription.usage_threshold_reached`.
pub async fn check_usage_thresholds(
    db: &Db,
    subscription: &Subscription,
    today: NaiveDate,
) -> Result<Option<Invoice>, JobError> {
    let plan = db.find_plan(subscription.plan_id).await?;
    let thresholds = db.effective_usage_thresholds(subscription.id, plan.id).await?;
    if thresholds.is_empty() {
        return Ok(None);
    }

    // The usage period in progress (for pay-in-advance plans too: their
    // usage is billed at the end of the period it accrued in).
    let (from_date, to_date) = crate::scheduler::usage_period(&subscription, &plan, today);
    let from = from_date.and_hms_opt(0, 0, 0).unwrap_or_default();
    let to = to_date.and_hms_opt(23, 59, 59).unwrap_or_default();
    let events_to = Utc::now().naive_utc().min(to);

    // Price the period's usage so far, charge by charge.
    let currency = plan.currency();
    let loaded = load_charges(db, subscription, &plan, from, to, events_to).await?;
    let mut current: Vec<Fee> = Vec::with_capacity(loaded.len());
    for (metric, charge, events, period) in &loaded {
        if !charge.invoiceable {
            continue;
        }
        current.push(rago_billing::charge_to_fee_in_period(
            metric,
            charge,
            subscription,
            events,
            currency,
            None,
            Some(period),
        )?);
    }
    let current_usage: i64 = current.iter().map(|f| f.amount_cents).sum();

    // What progressive invoices already billed this period, per charge line.
    let rows = db.progressive_invoices_for_period(subscription.id, from, to).await?;
    let already_billed: i64 = rows.iter().map(|r| r.usage_amount_cents).sum();
    let mut billed_per_line: HashMap<(Uuid, Option<Uuid>, String), i64> = HashMap::new();
    for row in &rows {
        for fee in db.fees_for_invoice(row.invoice_id).await? {
            if let (true, Some(charge_id)) = (fee.is_charge(), fee.charge_id) {
                *billed_per_line
                    .entry((charge_id, fee.charge_filter_id, fee.grouped_by.to_string()))
                    .or_default() += fee.amount_cents;
            }
        }
    }

    let invoiced = db.invoiced_usage_amount_cents(subscription.id, from).await?;
    let usage_thresholds: Vec<UsageThreshold> = thresholds
        .iter()
        .map(|t| UsageThreshold { id: t.id, amount_cents: t.amount_cents, recurring: t.recurring })
        .collect();
    let passed = check_thresholds(
        &usage_thresholds,
        LifetimeUsage {
            historical_usage_amount_cents: 0,
            invoiced_usage_amount_cents: invoiced,
            current_usage_amount_cents: current_usage,
        },
        already_billed,
    );
    if passed.is_empty() {
        return Ok(None);
    }

    // Bill only the usage not yet billed progressively this period.
    let invoice_id = Uuid::new_v4();
    let mut fees: Vec<Fee> = current
        .into_iter()
        .filter_map(|fee| {
            let key = (fee.charge_id?, fee.charge_filter_id, fee.grouped_by.to_string());
            let delta = fee.amount_cents - billed_per_line.get(&key).copied().unwrap_or(0);
            (delta > 0).then(|| Fee {
                id: Uuid::new_v4(),
                invoice_id: Some(invoice_id),
                amount_cents: delta,
                precise_amount_cents: Decimal::from(delta),
                ..fee
            })
        })
        .collect();
    if fees.is_empty() {
        return Ok(None);
    }
    let usage_billed: i64 = fees.iter().map(|f| f.amount_cents).sum();

    let mut invoice = progressive_invoice(subscription, invoice_id, &currency, today);
    let taxes = db.organization_taxes(subscription.organization_id).await?;
    assemble_invoice(&mut invoice, &mut fees, &taxes, &[]);
    invoice.payment_status = if invoice.total_amount_cents > 0 {
        PaymentStatus::Pending
    } else {
        PaymentStatus::Succeeded
    };

    let threshold_ids: Vec<Uuid> = passed.iter().map(|t| t.id).collect();
    let written = db
        .insert_progressive_invoice(
            &mut invoice,
            &fees,
            subscription.id,
            from,
            to,
            usage_billed,
            &threshold_ids,
            already_billed,
        )
        .await?;
    if !written {
        // A concurrent check billed this usage first.
        return Ok(None);
    }

    let invoice = crate::finalize::apply_credit_notes(db, invoice).await?;
    let org = subscription.organization_id;
    record_webhook(db, org, WebhookKind::InvoiceCreated, invoice_webhook_object(&invoice)).await?;
    record_invoice_generated(db, org, invoice.id, invoice_webhook_object(&invoice)).await?;
    for fee in &fees {
        record_webhook(db, org, WebhookKind::FeeCreated, fee_webhook_object(fee, &invoice)).await?;
    }
    for t in &passed {
        let row = thresholds.iter().find(|r| r.id == t.id);
        record_webhook(
            db,
            org,
            WebhookKind::SubscriptionUsageThresholdReached,
            json!({
                "lago_id": subscription.id,
                "external_id": subscription.external_id,
                "lago_customer_id": subscription.customer_id,
                "lago_invoice_id": invoice.id,
                "usage_threshold": {
                    "lago_id": t.id,
                    "threshold_display_name": row.and_then(|r| r.threshold_display_name.clone()),
                    "amount_cents": t.amount_cents,
                    "recurring": t.recurring,
                },
            }),
        )
        .await?;
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

fn progressive_invoice(
    subscription: &Subscription,
    id: Uuid,
    currency: &Currency,
    issuing_date: NaiveDate,
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
        invoice_type: InvoiceType::ProgressiveBilling,
        status: InvoiceStatus::Finalized,
        payment_status: PaymentStatus::Pending,
        currency: Some(currency.code()),
        issuing_date: Some(issuing_date),
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

/// `CreditNotes::CreateFromProgressiveBillingInvoice` — the true-up: when a
/// progressive invoice billed more usage than the period finally charged, the
/// excess is credited to the customer as a credit note on that invoice.
pub(crate) async fn issue_true_up_credit_note(
    db: &Db,
    note: &ProgressiveCreditNote,
) -> Result<(), JobError> {
    if note.amount_cents <= 0 {
        return Ok(());
    }
    let Some(invoice) = db.find_invoice(note.progressive_billing_invoice_id).await? else {
        return Ok(());
    };
    let Some(customer_id) = invoice.customer_id else {
        return Ok(());
    };
    let currency = invoice.currency.clone().unwrap_or_else(|| "USD".into());
    let taxes_amount_cents = ((note.amount_cents as f64) * invoice.taxes_rate / 100.0).round() as i64;
    let total = note.amount_cents + taxes_amount_cents;
    let seq = db.next_credit_note_sequential_id(invoice.id).await?;
    let now = Utc::now();
    let cn = CreditNote {
        id: Uuid::new_v4(),
        organization_id: invoice.organization_id,
        customer_id,
        invoice_id: invoice.id,
        sequential_id: seq,
        number: format!("{}-CN{seq}", invoice.number),
        issuing_date: now.date_naive(),
        // `other`
        reason: 5,
        credit_status: Some(0),
        refund_status: None,
        status: 1,
        credit_amount_cents: total,
        credit_amount_currency: currency.clone(),
        balance_amount_cents: total,
        refund_amount_cents: 0,
        total_amount_cents: total,
        total_amount_currency: currency.clone(),
        taxes_amount_cents,
        taxes_rate: invoice.taxes_rate,
        coupons_adjustment_amount_cents: 0,
        precise_coupons_adjustment_amount_cents: Decimal::ZERO,
        precise_taxes_amount_cents: Decimal::from(taxes_amount_cents),
        created_at: now,
        updated_at: now,
        voided_at: None,
        items: Vec::new(),
        applied_taxes: Vec::new(),
    };
    db.insert_credit_note(&cn).await?;
    let object = json!({
        "lago_id": cn.id,
        "number": cn.number,
        "lago_invoice_id": cn.invoice_id,
        "total_amount_cents": cn.total_amount_cents,
        "currency": currency,
        "reason": "other",
    });
    record_webhook(db, cn.organization_id, WebhookKind::CreditNoteCreated, object.clone()).await?;
    record_credit_note_generated(db, cn.organization_id, cn.id, object).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_billing_date_is_first_of_next_month_for_calendar_monthly() {
        let sub_at = NaiveDate::from_ymd_opt(2026, 1, 15).unwrap();
        let today = NaiveDate::from_ymd_opt(2026, 9, 10).unwrap();
        let next = next_billing_date(Interval::Monthly, false, sub_at, today);
        assert_eq!(next, NaiveDate::from_ymd_opt(2026, 10, 1).unwrap());
    }
}
