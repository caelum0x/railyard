//! `BillSubscriptionJob` — bill one subscription for a usage window.

use chrono::{NaiveDateTime, Utc};
use rago_billing::dates::MonthlyDates;
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
pub async fn perform(
    db: &Db,
    subscription_id: Uuid,
    from: NaiveDateTime,
    to: NaiveDateTime,
) -> Result<BillingRun, JobError> {
    let subscription = db.find_subscription(subscription_id).await?;
    let plan = db.find_plan(subscription.plan_id).await?;
    let currency = plan.currency();
    let charges = db.charges_for_plan(plan.id).await?;
    let taxes = db.organization_taxes(subscription.organization_id).await?;

    let now = Utc::now();
    let mut invoice = blank_invoice(&subscription, &currency, now);

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

    // Billing-period dates (used for subscription-fee proration).
    let dates = MonthlyDates {
        subscription_at: subscription
            .subscription_at
            .map(|t| t.date_naive())
            .unwrap_or_else(|| from.date()),
        billing_at: to.date() + chrono::Duration::days(1),
        anniversary: matches!(
            subscription.billing_time,
            rago_core::enums::BillingTime::Anniversary
        ),
        pay_in_advance: plan.pay_in_advance,
        current_usage: false,
    };

    // Full generation: subscription fee + charge fees + taxes (no coupons).
    let fees = generate_subscription_invoice(
        &mut invoice,
        &plan,
        &subscription,
        &dates,
        Proration::Full,
        &charge_inputs,
        &taxes,
        &[],
    )?;

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
        rago_db::BilledInvoice::Created => Ok(BillingRun { invoice, fees }),
        rago_db::BilledInvoice::AlreadyBilled(existing) => {
            let existing_fees = db.fees_for_invoice(existing.id).await?;
            Ok(BillingRun {
                invoice: existing,
                fees: existing_fees,
            })
        }
    }
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
