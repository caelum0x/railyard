//! Billing scheduler runtime — the auto-biller.
//!
//! Port of the `Clock::SubscriptionsBillerJob` → `OrganizationBillingService` →
//! `BillSubscriptionJob` pipeline: on each tick, find the subscriptions whose
//! billing period rolls over on `billing_date` (via
//! [`rago_billing::dates::due_for_billing`]), bill each one into an invoice,
//! emit the `invoice.created` webhook, charge the customer through the payment
//! provider, and emit the payment-status webhook.

use chrono::{Datelike, NaiveDate, NaiveDateTime, Utc};
use rago_billing::aggregation::AggregationPeriod;
use rago_billing::dates::{due_for_billing, Interval, MonthlyDates, PeriodDates};
use rago_billing::generate::{generate_subscription_invoice_with_progressive, ChargeInput};
use rago_billing::progressive_billing::{ProgressiveBillingInvoice, ProgressiveFee};
use rago_billing::subscription_fee::Proration;
use rago_core::enums::{InvoiceStatus, InvoiceType, PaymentStatus, PlanInterval};
use rago_core::models::{Fee, Invoice, Subscription};
use rago_core::money::Currency;
use rago_db::Db;
use rago_integrations::webhooks::WebhookKind;
use rago_integrations::{HttpTransport, PayableStatus};
use serde_json::json;
use uuid::Uuid;

use crate::collection::{collect_invoice, CollectOutcome};
use crate::webhook_emit::record_webhook;
use crate::JobError;

/// Scheduler configuration.
pub struct SchedulerConfig {
    /// Whether to collect payment after invoicing. The provider, its
    /// credentials and each customer's provider account are resolved per
    /// organization (see [`crate::collection`]); nothing here is shared across
    /// tenants.
    pub charge_customers: bool,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self { charge_customers: true }
    }
}

/// What a billing cycle produced.
#[derive(Debug, Default)]
pub struct CycleReport {
    pub billed_invoices: Vec<Uuid>,
    pub payments_succeeded: usize,
    pub payments_failed: usize,
    pub payments_skipped: usize,
    pub webhooks_written: usize,
    /// Subscriptions whose billing or collection errored (logged; the cycle
    /// continues with the next subscription).
    pub errors: usize,
}

/// Run one billing cycle for `billing_date`. When `force` is set, every active
/// subscription is billed regardless of its due date (used for on-demand runs);
/// otherwise only subscriptions due on `billing_date` are billed. A failure on
/// one subscription (billing error, provider outage) is logged and counted but
/// never stops the cycle for the remaining subscriptions.
pub async fn run_billing_cycle<T: HttpTransport>(
    db: &Db,
    transport: &T,
    cfg: &SchedulerConfig,
    billing_date: NaiveDate,
    force: bool,
) -> Result<CycleReport, JobError> {
    let mut report = CycleReport::default();

    for subscription in db.active_subscriptions().await? {
        let plan = db.find_plan(subscription.plan_id).await?;
        let interval = plan_interval(&plan.interval);
        let anniversary = matches!(
            subscription.billing_time,
            rago_core::enums::BillingTime::Anniversary
        );
        let sub_at = subscription
            .subscription_at
            .map(|t| t.date_naive())
            .unwrap_or(billing_date);

        if !force && !due_for_billing(interval, anniversary, sub_at, billing_date) {
            continue;
        }

        match bill_and_collect(db, transport, &subscription, &plan, interval, anniversary, billing_date, cfg.charge_customers).await {
            Ok(outcome) => {
                report.billed_invoices.push(outcome.invoice_id);
                report.webhooks_written += outcome.webhooks_written;
                match outcome.collection {
                    Some(CollectOutcome::Charged(PayableStatus::Succeeded)) => report.payments_succeeded += 1,
                    Some(CollectOutcome::Charged(PayableStatus::Failed)) => report.payments_failed += 1,
                    Some(CollectOutcome::Charged(PayableStatus::Pending)) => {}
                    Some(CollectOutcome::Skipped(_)) => report.payments_skipped += 1,
                    None => {}
                }
            }
            Err(e) => {
                report.errors += 1;
                tracing::error!(
                    subscription_id = %subscription.id,
                    organization_id = %subscription.organization_id,
                    error = %e,
                    "billing/collection failed for subscription; continuing cycle"
                );
            }
        }
    }

    Ok(report)
}

/// The result of billing + collecting one subscription.
#[derive(Debug)]
pub struct BillOutcome {
    pub invoice_id: Uuid,
    /// `true` when this run created the invoice (vs. found it already billed).
    pub created: bool,
    pub webhooks_written: usize,
    pub collection: Option<CollectOutcome>,
}

/// Bill one subscription for the period ending at `billing_date`, queue the
/// `invoice.created` + `fee.created` webhooks (only when the invoice is new),
/// then collect payment for it. Shared by the in-process cycle and the durable
/// `bill_subscription` job so both do the full Lago pipeline.
#[allow(clippy::too_many_arguments)]
pub async fn bill_and_collect<T: HttpTransport>(
    db: &Db,
    transport: &T,
    subscription: &Subscription,
    plan: &rago_core::models::Plan,
    interval: Interval,
    anniversary: bool,
    billing_date: NaiveDate,
    charge: bool,
) -> Result<BillOutcome, JobError> {
    let (invoice, fees, created) =
        bill_one(db, subscription, plan, interval, anniversary, billing_date).await?;

    let mut webhooks_written = 0;
    if created {
        if record_webhook(
            db,
            subscription.organization_id,
            WebhookKind::InvoiceCreated,
            invoice_webhook_object(&invoice),
        )
        .await?
        {
            webhooks_written += 1;
        }
        if matches!(invoice.status, rago_core::enums::InvoiceStatus::Finalized) {
            // The finalized invoice's document is downloadable from now on.
            crate::webhook_emit::record_invoice_generated(
                db,
                subscription.organization_id,
                invoice.id,
                invoice_webhook_object(&invoice),
            )
            .await?;
        }
        for fee in &fees {
            if record_webhook(
                db,
                subscription.organization_id,
                WebhookKind::FeeCreated,
                fee_webhook_object(fee, &invoice),
            )
            .await?
            {
                webhooks_written += 1;
            }
        }
    }

    // Collect. A re-run for an already-billed invoice only collects when no
    // attempt was ever recorded (e.g. the earlier run hit an outage before
    // reaching the provider); retries of declined charges belong to dunning /
    // the operator, not to billing re-runs. The charge guard prevents any
    // double charge either way.
    let never_attempted = created
        || db
            .payment_attempt_state(crate::payments::deterministic_payment_id(invoice.id))
            .await?
            .is_none();
    let collection = if charge && invoice.total_amount_cents > 0 && never_attempted {
        Some(collect_invoice(db, transport, &invoice).await?)
    } else {
        None
    };

    Ok(BillOutcome {
        invoice_id: invoice.id,
        created,
        webhooks_written,
        collection,
    })
}

/// Public entrypoint used by the durable-queue worker: bill a subscription
/// (resolved by external id) for the period ending at `billing_date`.
pub async fn bill_subscription_external(
    db: &Db,
    organization_id: Uuid,
    external_id: &str,
    billing_date: NaiveDate,
) -> Result<(Invoice, Vec<Fee>), JobError> {
    let sub = db
        .find_subscription_by_external_id(organization_id, external_id)
        .await?
        .ok_or_else(|| JobError::Payment("subscription_not_found".into()))?;
    let plan = db.find_plan(sub.plan_id).await?;
    let interval = plan_interval(&plan.interval);
    let anniversary = matches!(sub.billing_time, rago_core::enums::BillingTime::Anniversary);
    let (invoice, fees, _) = bill_one(db, &sub, &plan, interval, anniversary, billing_date).await?;
    Ok((invoice, fees))
}

/// Durable-queue entrypoint: bill the subscription (resolved by external id),
/// queue its webhooks, and collect payment — the full pipeline.
pub async fn bill_and_collect_external<T: HttpTransport>(
    db: &Db,
    transport: &T,
    organization_id: Uuid,
    external_id: &str,
    billing_date: NaiveDate,
) -> Result<BillOutcome, JobError> {
    let sub = db
        .find_subscription_by_external_id(organization_id, external_id)
        .await?
        .ok_or_else(|| JobError::Payment("subscription_not_found".into()))?;
    let plan = db.find_plan(sub.plan_id).await?;
    let interval = plan_interval(&plan.interval);
    let anniversary = matches!(sub.billing_time, rago_core::enums::BillingTime::Anniversary);
    bill_and_collect(db, transport, &sub, &plan, interval, anniversary, billing_date, true).await
}

/// Charges-period boundaries `(from_date, to_date)` of the period that rolls
/// over on `billing_date`. Monthly uses the full `MonthlyDates` logic, the
/// other intervals `PeriodDates` (calendar).
pub(crate) fn period_bounds(
    plan: &rago_core::models::Plan,
    interval: Interval,
    anniversary: bool,
    sub_at: NaiveDate,
    billing_date: NaiveDate,
) -> (NaiveDate, NaiveDate) {
    if interval == Interval::Monthly {
        let b = MonthlyDates {
            subscription_at: sub_at,
            billing_at: billing_date,
            anniversary,
            pay_in_advance: plan.pay_in_advance,
            current_usage: false,
        }
        .boundaries();
        (b.from_date, b.to_date)
    } else {
        let b = PeriodDates {
            interval,
            subscription_at: sub_at,
            billing_at: billing_date,
            anniversary,
            pay_in_advance: plan.pay_in_advance,
        }
        .boundaries();
        (b.from_date, b.to_date)
    }
}

/// A usage charge loaded for billing: metric, charge, the period's events and
/// the aggregation period (with the weighted-sum carry-in for recurring metrics).
pub(crate) type LoadedCharge = (
    rago_core::models::BillableMetric,
    rago_core::models::Charge,
    Vec<rago_core::models::Event>,
    AggregationPeriod,
);

/// Load every usage charge of `plan` with the subscription's events in
/// `[from, events_to]`, aggregated over the full period `[from, to]`.
pub(crate) async fn load_charges(
    db: &Db,
    subscription: &Subscription,
    plan: &rago_core::models::Plan,
    from: NaiveDateTime,
    to: NaiveDateTime,
    events_to: NaiveDateTime,
) -> Result<Vec<LoadedCharge>, JobError> {
    let charges = db.charges_for_plan(plan.id).await?;
    let mut loaded: Vec<LoadedCharge> = Vec::with_capacity(charges.len());
    for charge in charges {
        // Pay-in-advance charges are billed when their events are ingested
        // (`pay_in_advance::bill_event_in_advance`), never at period end.
        if charge.pay_in_advance {
            continue;
        }
        let Some(mid) = charge.billable_metric_id else { continue };
        let metric = db.find_billable_metric(mid).await?;
        let events = db
            .events_for_external_subscription(
                subscription.organization_id,
                &subscription.external_id,
                &metric.code,
                from,
                events_to,
            )
            .await?;
        let carry_in = db
            .weighted_sum_carry_in(
                subscription.organization_id,
                &subscription.external_id,
                &metric,
                from,
            )
            .await?;
        let period = AggregationPeriod::new(from, to).with_initial_value(carry_in);
        loaded.push((metric, charge, events, period));
    }
    Ok(loaded)
}

/// Progressive invoices of `(subscription, [from, to])` not yet credited back,
/// shaped for the credit-back service.
async fn uncredited_progressive_invoices(
    db: &Db,
    subscription_id: Uuid,
    from: NaiveDateTime,
    to: NaiveDateTime,
) -> Result<(Vec<Uuid>, Vec<ProgressiveBillingInvoice>), JobError> {
    let mut row_ids = Vec::new();
    let mut invoices = Vec::new();
    for row in db.progressive_invoices_for_period(subscription_id, from, to).await? {
        if row.credited_invoice_id.is_some() {
            continue;
        }
        let Some(inv) = db.find_invoice(row.invoice_id).await? else { continue };
        let fees = db.fees_for_invoice(inv.id).await?;
        invoices.push(ProgressiveBillingInvoice {
            id: inv.id,
            fees_amount_cents: inv.fees_amount_cents,
            coupons_amount_cents: inv.coupons_amount_cents,
            progressive_billing_credits_amount_cents: 0,
            credit_notes_amount_cents: inv.credit_notes_amount_cents,
            fees: fees
                .iter()
                .filter(|f| f.is_charge())
                .filter_map(|f| {
                    Some(ProgressiveFee {
                        charge_id: f.charge_id?,
                        charge_filter_id: f.charge_filter_id,
                        grouped_by: f.grouped_by.clone(),
                        amount_cents: f.amount_cents,
                        creditable_amount_cents: f.amount_cents,
                    })
                })
                .collect(),
        });
        row_ids.push(row.id);
    }
    Ok((row_ids, invoices))
}

/// Bill a single subscription for the period ending at `billing_date`.
async fn bill_one(
    db: &Db,
    subscription: &Subscription,
    plan: &rago_core::models::Plan,
    interval: Interval,
    anniversary: bool,
    billing_date: NaiveDate,
) -> Result<(Invoice, Vec<Fee>, bool), JobError> {
    let currency = plan.currency();
    let sub_at = subscription
        .subscription_at
        .map(|t| t.date_naive())
        .unwrap_or(billing_date);

    let (from_date, to_date) = period_bounds(plan, interval, anniversary, sub_at, billing_date);
    let from = from_date.and_hms_opt(0, 0, 0).unwrap_or_default();
    let to = to_date.and_hms_opt(23, 59, 59).unwrap_or_default();

    let loaded = load_charges(db, subscription, plan, from, to, to).await?;
    let charge_inputs: Vec<ChargeInput> = loaded
        .iter()
        .map(|(m, c, e, p)| ChargeInput { metric: m, charge: c, events: e, period: Some(*p) })
        .collect();

    // Usage already billed mid-period by progressive invoices is credited back.
    let (progressive_row_ids, progressive) =
        uncredited_progressive_invoices(db, subscription.id, from, to).await?;

    let taxes = db.organization_taxes(subscription.organization_id).await?;
    let mut invoice = blank_invoice(subscription, &currency, to_date);
    let (fees, progressive_results) = generate_subscription_invoice_with_progressive(
        &mut invoice,
        plan,
        subscription,
        &monthly_for_single_day(sub_at, billing_date, anniversary, plan.pay_in_advance),
        Proration::Full,
        &charge_inputs,
        &taxes,
        &[],
        &progressive,
    )?;

    // Apply prepaid wallet credit (after tax) before persisting, so the invoice
    // total reflects the customer's prepaid balance. Only committed on the
    // winning re-bill reservation, so the wallet is drawn down exactly once.
    let settlement =
        crate::prepaid_settle::settle_prepaid(db, subscription.customer_id, &mut invoice, &fees)
            .await?;
    let prepaid = rago_db::PrepaidSettlement {
        transactions: &settlement.transactions,
        updated_wallets: &settlement.updated_wallets,
    };

    // Re-bill guard: at most one recurring invoice per (subscription, period).
    // A re-run for the same subscription+period returns the existing invoice
    // instead of creating a duplicate (which the payment path would double-charge).
    match db
        .insert_subscription_invoice_with_prepaid_guarded(
            &mut invoice, &fees, &prepaid, subscription.id, from, to,
        )
        .await?
    {
        rago_db::BilledInvoice::Created => {
            db.mark_progressive_invoices_credited(&progressive_row_ids, invoice.id).await?;
            for result in &progressive_results {
                if let Some(note) = &result.credit_note {
                    crate::progressive_billing::issue_true_up_credit_note(db, note).await?;
                }
            }
            Ok((invoice, fees, true))
        }
        rago_db::BilledInvoice::AlreadyBilled(existing) => {
            let existing_fees = db.fees_for_invoice(existing.id).await?;
            Ok((existing, existing_fees, false))
        }
    }
}

fn plan_interval(interval: &Option<PlanInterval>) -> Interval {
    match interval {
        Some(PlanInterval::Weekly) => Interval::Weekly,
        Some(PlanInterval::Quarterly) => Interval::Quarterly,
        Some(PlanInterval::Semiannual) => Interval::Semiannual,
        Some(PlanInterval::Yearly) => Interval::Yearly,
        _ => Interval::Monthly,
    }
}

/// Public alias for the billing `Interval` of a plan (used by the queue).
pub fn interval_of(interval: &Option<PlanInterval>) -> Interval {
    plan_interval(interval)
}

/// The subscription fee needs a `MonthlyDates` for single-day-price proration;
/// for non-monthly plans this is only used when prorating (not here, where we
/// bill the full period), so a monthly view suffices.
fn monthly_for_single_day(
    sub_at: NaiveDate,
    billing_at: NaiveDate,
    anniversary: bool,
    pay_in_advance: bool,
) -> MonthlyDates {
    MonthlyDates {
        subscription_at: sub_at,
        billing_at,
        anniversary,
        pay_in_advance,
        current_usage: false,
    }
}

pub(crate) fn fee_webhook_object(fee: &Fee, invoice: &Invoice) -> serde_json::Value {
    json!({
        "lago_id": fee.id,
        "lago_invoice_id": invoice.id,
        "lago_subscription_id": fee.subscription_id,
        "lago_charge_id": fee.charge_id,
        "amount_cents": fee.amount_cents,
        "amount_currency": fee.amount_currency,
        "taxes_amount_cents": fee.taxes_amount_cents,
        "units": fee.units.to_string(),
        "events_count": fee.events_count,
        "payment_status": fee.payment_status.as_str(),
    })
}

pub(crate) fn invoice_webhook_object(invoice: &Invoice) -> serde_json::Value {
    json!({
        "lago_id": invoice.id,
        "number": invoice.number,
        "status": invoice.status.as_str(),
        "payment_status": invoice.payment_status.as_str(),
        "currency": invoice.currency,
        "fees_amount_cents": invoice.fees_amount_cents,
        "taxes_amount_cents": invoice.taxes_amount_cents,
        "total_amount_cents": invoice.total_amount_cents,
    })
}

fn blank_invoice(sub: &Subscription, currency: &Currency, issuing_date: NaiveDate) -> Invoice {
    let now = Utc::now();
    Invoice {
        id: Uuid::new_v4(),
        organization_id: sub.organization_id,
        billing_entity_id: sub.billing_entity_id.unwrap_or(sub.organization_id),
        customer_id: Some(sub.customer_id),
        // Assigned from the per-organization sequence at insert time.
        number: String::new(),
        sequential_id: None,
        invoice_type: InvoiceType::Subscription,
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

/// A single scheduler tick's date, in UTC.
pub fn today() -> NaiveDate {
    Utc::now().date_naive()
}

/// First day of the month following `date` (the calendar-monthly billing
/// anchor: the period that closed yesterday is billed today).
pub fn first_of_next_month(date: NaiveDate) -> NaiveDate {
    if date.month() == 12 {
        NaiveDate::from_ymd_opt(date.year() + 1, 1, 1).unwrap()
    } else {
        NaiveDate::from_ymd_opt(date.year(), date.month() + 1, 1).unwrap()
    }
}
