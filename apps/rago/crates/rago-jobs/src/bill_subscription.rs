//! `BillSubscriptionJob` — bill one subscription for an explicit usage window.
//!
//! The date-driven pipeline (billing cycle, durable `bill_subscription` job)
//! goes through [`crate::scheduler::bill_and_collect`]; this entry point bills
//! an arbitrary `[from, to)` window (backfills, operator re-bills). Both follow
//! one draft rule: the invoice is a draft only while the customer's (else the
//! organization's) grace period is open, and is finalized — numbered, taxed,
//! announced and collected — by [`crate::finalize`] otherwise.

use chrono::{Duration, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use rago_billing::generate::{generate_subscription_invoice, ChargeInput};
use rago_billing::subscription_fee::Proration;
use rago_core::enums::{InvoiceStatus, InvoiceType, PaymentStatus};
use rago_core::models::{Fee, Invoice};
use rago_db::Db;
use uuid::Uuid;

use crate::JobError;

/// Outcome of a billing run: the assembled (persisted) invoice and its fees.
pub struct BillingRun {
    pub invoice: Invoice,
    pub fees: Vec<Fee>,
}

/// Perform the in-arrears billing for `subscription_id` over `[from, to)`.
///
/// Reads the subscription, its plan and charges, aggregates each charge's
/// events, prices them into fees, assembles the invoice totals (with the
/// organization's taxes), and persists invoice + fees.
///
/// The window is dated with the plan's own interval (weekly .. yearly,
/// calendar or anniversary): the subscription fee is the fee of the period
/// containing `from`, in full when the window covers that whole period and
/// prorated by day otherwise. A window must stay inside one billing period
/// (each period has its own invoice); a longer backfill is billed one period at
/// a time.
pub async fn perform(
    db: &Db,
    subscription_id: Uuid,
    from: NaiveDateTime,
    to: NaiveDateTime,
) -> Result<BillingRun, JobError> {
    let subscription = db.find_subscription(subscription_id).await?;
    let plan = db.find_plan(subscription.plan_id).await?;
    let (first_day, last_day) = window_days(from, to);
    let period = crate::scheduler::span_of(&subscription, &plan, first_day).window(first_day, last_day)?;
    let proration = period.fee_proration();
    let currency = plan.currency();
    let charges = db.charges_for_plan(plan.id).await?;
    let taxes = db.organization_taxes(subscription.organization_id).await?;

    let now = Utc::now();
    let mut invoice = blank_invoice(&subscription, &currency, now);
    let grace = db
        .invoice_grace_period_for(subscription.organization_id, Some(subscription.customer_id))
        .await?;
    if grace <= 0 {
        invoice.status = InvoiceStatus::Finalized;
    }

    // Load each charge's metric + events so the generation service can price
    // the flat subscription fee and one fee per usage charge together.
    let mut loaded: Vec<(
        rago_core::models::BillableMetric,
        rago_core::models::Charge,
        Vec<rago_core::models::Event>,
        rago_billing::aggregation::AggregationPeriod,
    )> = Vec::new();
    for charge in &charges {
        // Billed at event ingest (`pay_in_advance`), not at period end.
        if charge.pay_in_advance {
            continue;
        }
        let Some(metric_id) = charge.billable_metric_id else {
            continue;
        };
        let metric = db.find_billable_metric(metric_id).await?;
        let events = db
            .events_in_period(subscription.id, &metric.code, from, to)
            .await?;
        let carry_in = db
            .weighted_sum_carry_in(
                subscription.organization_id,
                &subscription.external_id,
                &metric,
                from,
            )
            .await?;
        let period =
            rago_billing::aggregation::AggregationPeriod::new(from, to).with_initial_value(carry_in);
        loaded.push((metric, charge.clone(), events, period));
    }

    let charge_inputs: Vec<ChargeInput> = loaded
        .iter()
        .map(|(metric, charge, events, period)| ChargeInput {
            metric,
            charge,
            events,
            period: Some(*period),
        })
        .collect();

    // Full generation: subscription fee (interval-aware, prorated to the
    // window) + charge fees + taxes (no coupons).
    let mut fees = generate_subscription_invoice(
        &mut invoice,
        &plan,
        &subscription,
        &period,
        proration,
        &charge_inputs,
        &taxes,
        &[],
    )?;
    if matches!(proration, Proration::Zero) {
        // No fee owed in the window (a trial covers it): no subscription line.
        fees.retain(|f| !matches!(f.fee_type, Some(rago_core::enums::FeeType::Subscription)));
    }

    // Apply prepaid wallet credit (after tax): draws the customer's wallet(s)
    // down against this invoice and reduces its total, matching Lago's
    // `AppliedPrepaidCreditsService` at invoice creation. The draw-down is only
    // *persisted* on the winning re-bill reservation below, so it is applied
    // exactly once per period.
    let settlement =
        crate::prepaid_settle::settle_prepaid(db, subscription.customer_id, &mut invoice, &fees)
            .await?;
    let prepaid = rago_db::PrepaidSettlement {
        transactions: &settlement.transactions,
        updated_wallets: &settlement.updated_wallets,
    };

    // Persist behind the re-bill guard: billing the same subscription+period
    // twice yields exactly one invoice. A re-run returns the existing invoice.
    match db
        .insert_subscription_invoice_with_prepaid_guarded(
            &mut invoice,
            &fees,
            &prepaid,
            subscription.id,
            from,
            to,
        )
        .await?
    {
        rago_db::BilledInvoice::Created => {
            if matches!(invoice.status, InvoiceStatus::Draft) {
                crate::webhook_emit::record_webhook(
                    db,
                    invoice.organization_id,
                    rago_integrations::webhooks::WebhookKind::InvoiceDrafted,
                    crate::scheduler::invoice_webhook_object(&invoice),
                )
                .await?;
                Ok(BillingRun { invoice, fees })
            } else {
                let invoice = crate::finalize::on_invoice_finalized(db, invoice, true).await?;
                let fees = db.fees_for_invoice(invoice.id).await?;
                Ok(BillingRun { invoice, fees })
            }
        }
        rago_db::BilledInvoice::AlreadyBilled(existing) => {
            let existing_fees = db.fees_for_invoice(existing.id).await?;
            Ok(BillingRun {
                invoice: existing,
                fees: existing_fees,
            })
        }
    }
}

/// The billed days of a `[from, to)` window: `to` at exactly midnight is an
/// exclusive end (the day before is the last day billed); any other time is
/// inside the last day (`23:59:59` as the scheduler writes it).
pub(crate) fn window_days(from: NaiveDateTime, to: NaiveDateTime) -> (NaiveDate, NaiveDate) {
    let first = from.date();
    let last = if to.time() == NaiveTime::MIN && to.date() > first {
        to.date() - Duration::days(1)
    } else {
        to.date()
    };
    (first, last)
}

fn blank_invoice(
    subscription: &rago_core::models::Subscription,
    currency: &rago_core::money::Currency,
    now: chrono::DateTime<Utc>,
) -> Invoice {
    Invoice {
        id: Uuid::new_v4(),
        organization_id: subscription.organization_id,
        billing_entity_id: subscription
            .billing_entity_id
            .unwrap_or(subscription.organization_id),
        customer_id: Some(subscription.customer_id),
        number: String::new(),
        sequential_id: None,
        invoice_type: InvoiceType::Subscription,
        status: InvoiceStatus::Draft,
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

#[cfg(test)]
mod tests {
    use super::window_days;
    use chrono::NaiveDate;

    fn at(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> chrono::NaiveDateTime {
        NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, mi, s).unwrap()
    }

    #[test]
    fn inclusive_and_exclusive_window_ends_bill_the_same_days() {
        let first = NaiveDate::from_ymd_opt(2026, 1, 1).unwrap();
        let last = NaiveDate::from_ymd_opt(2026, 3, 31).unwrap();
        assert_eq!(window_days(at(2026, 1, 1, 0, 0, 0), at(2026, 3, 31, 23, 59, 59)), (first, last));
        assert_eq!(window_days(at(2026, 1, 1, 0, 0, 0), at(2026, 4, 1, 0, 0, 0)), (first, last));
    }

    #[test]
    fn a_single_day_window_keeps_its_day() {
        let day = NaiveDate::from_ymd_opt(2026, 5, 4).unwrap();
        assert_eq!(window_days(at(2026, 5, 4, 0, 0, 0), at(2026, 5, 4, 0, 0, 0)), (day, day));
    }
}
