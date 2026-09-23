//! `Invoices::CalculateFeesService` (core path) — assemble a full subscription
//! invoice from real charges + events: the flat subscription fee plus one fee
//! per usage charge, then coupons, taxes and totals.
//!
//! This is the in-memory end-to-end generation path (no persistence); the
//! `rago-jobs` `BillSubscriptionJob` wires it to the database.

use rago_core::models::{BillableMetric, Charge, Event, Fee, Invoice, Plan, Subscription, Tax};
use rago_core::money::Currency;

use crate::aggregation::AggregationPeriod;
use crate::charge_to_fee_in_period;
use crate::dates::MonthlyDates;
use crate::error::Result;
use crate::invoices::{assemble_invoice, CouponApplication};
use crate::progressive_billing::{
    apply_progressive_billing_credit, ProgressiveBillingInvoice, ProgressiveCreditResult,
};
use crate::subscription_fee::{build_subscription_fee, Proration};

/// One usage charge to bill: its metric, charge config, and period events.
pub struct ChargeInput<'a> {
    pub metric: &'a BillableMetric,
    pub charge: &'a Charge,
    pub events: &'a [Event],
    /// Billing period the events belong to (needed by `weighted_sum_agg`).
    pub period: Option<AggregationPeriod>,
}

/// Generate a subscription invoice end-to-end and mutate `invoice` with the
/// computed totals. Returns the fees (subscription fee first, then charge fees).
///
/// Mirrors `CalculateFeesService`: create the subscription fee, then a charge
/// fee per invoiceable charge, then `ComputeAmountsFromFees` (via
/// [`assemble_invoice`]) applying coupons, taxes and rolling up totals. The
/// invoice `payment_status` is set to succeeded when the total is zero.
#[allow(clippy::too_many_arguments)]
pub fn generate_subscription_invoice(
    invoice: &mut Invoice,
    plan: &Plan,
    subscription: &Subscription,
    dates: &MonthlyDates,
    proration: Proration,
    charges: &[ChargeInput],
    taxes: &[Tax],
    coupons: &[CouponApplication],
) -> Result<Vec<Fee>> {
    generate_subscription_invoice_with_progressive(
        invoice,
        plan,
        subscription,
        dates,
        proration,
        charges,
        taxes,
        coupons,
        &[],
    )
    .map(|(fees, _)| fees)
}

/// [`generate_subscription_invoice`] for a period that was partly billed
/// mid-period by progressive-billing invoices: their usage is credited back on
/// the matching charge fees before taxes (`Credits::ProgressiveBillingService`),
/// so the customer is never billed twice for the same usage. Returns the fees
/// and one credit result per progressive invoice (true-up credit notes to issue
/// when a progressive invoice billed more than the period's final usage).
#[allow(clippy::too_many_arguments)]
pub fn generate_subscription_invoice_with_progressive(
    invoice: &mut Invoice,
    plan: &Plan,
    subscription: &Subscription,
    dates: &MonthlyDates,
    proration: Proration,
    charges: &[ChargeInput],
    taxes: &[Tax],
    coupons: &[CouponApplication],
    progressive: &[ProgressiveBillingInvoice],
) -> Result<(Vec<Fee>, Vec<ProgressiveCreditResult>)> {
    let currency = plan.currency();
    let billing_entity_id = subscription
        .billing_entity_id
        .unwrap_or(subscription.organization_id);

    let mut fees: Vec<Fee> = Vec::with_capacity(charges.len() + 1);

    // Subscription (flat plan) fee.
    fees.push(build_subscription_fee(
        plan,
        subscription,
        Some(invoice.id),
        billing_entity_id,
        dates,
        proration,
    ));

    // Usage charge fees.
    for input in charges {
        if !input.charge.invoiceable {
            continue;
        }
        let fee = charge_to_fee_in_period(
            input.metric,
            input.charge,
            subscription,
            input.events,
            currency,
            Some(invoice.id),
            input.period.as_ref(),
        )?;
        fees.push(fee);
    }

    // Credit back usage already billed progressively this period (before taxes).
    let progressive_results: Vec<ProgressiveCreditResult> = progressive
        .iter()
        .map(|pbi| apply_progressive_billing_credit(invoice, &mut fees, pbi))
        .collect();

    assemble_invoice(invoice, &mut fees, taxes, coupons);

    // CalculateFeesService: zero-total invoices are marked paid.
    invoice.payment_status = if invoice.total_amount_cents > 0 {
        rago_core::enums::PaymentStatus::Pending
    } else {
        rago_core::enums::PaymentStatus::Succeeded
    };

    Ok((fees, progressive_results))
}

/// Convenience: build a [`Currency`] for a plan.
pub fn plan_currency(plan: &Plan) -> Currency {
    plan.currency()
}
