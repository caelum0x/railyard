//! `Invoices::CalculateFeesService` (core path) — assemble a full subscription
//! invoice from real charges + events: the flat subscription fee plus one fee
//! per usage charge, then coupons, taxes and totals.
//!
//! This is the in-memory end-to-end generation path (no persistence); the
//! `rago-jobs` `BillSubscriptionJob` wires it to the database.

use rago_core::models::{BillableMetric, Charge, Event, Fee, Invoice, Plan, Subscription, Tax};
use rago_core::money::Currency;

use crate::aggregation::AggregationPeriod;
use crate::charge_fees::{charge_fees_all, retain_used, ChargeUsage, FilterSpec};
use crate::commitments::charge_true_up;
use crate::proration::FeeDates;
use crate::error::Result;
use crate::invoices::{assemble_invoice, CouponApplication};
use crate::progressive_billing::{
    apply_progressive_billing_credit, ProgressiveBillingInvoice, ProgressiveCreditResult,
};
use crate::subscription_fee::{build_subscription_fee, Proration};

/// One usage charge to bill: its metric, charge config, filters and events.
pub struct ChargeInput<'a> {
    pub metric: &'a BillableMetric,
    pub charge: &'a Charge,
    /// The charge's live filters (empty when it has none).
    pub filters: &'a [FilterSpec],
    pub events: &'a [Event],
    /// The metric's events before the period, read by prorated charges on
    /// recurring metrics (see [`crate::charge_fees::needs_persisted_events`]).
    pub persisted_events: &'a [Event],
    /// Billing period the events belong to (needed by `weighted_sum_agg` and
    /// prorated charges).
    pub period: Option<AggregationPeriod>,
}

impl<'a> ChargeInput<'a> {
    /// The billing engine's view of this input.
    pub fn usage(&'a self) -> ChargeUsage<'a> {
        ChargeUsage {
            metric: self.metric,
            charge: self.charge,
            filters: self.filters,
            events: self.events,
            persisted_events: self.persisted_events,
            period: self.period.as_ref(),
        }
    }
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
    dates: &dyn FeeDates,
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
        Vec::new(),
    )
    .map(|(fees, _)| fees)
}

/// [`generate_subscription_invoice`] for a period that was partly billed
/// mid-period by progressive-billing invoices: their usage is credited back on
/// the matching charge fees before taxes (`Credits::ProgressiveBillingService`),
/// so the customer is never billed twice for the same usage. Returns the fees
/// and one credit result per progressive invoice (true-up credit notes to issue
/// when a progressive invoice billed more than the period's final usage).
/// `fixed_charge_fees` (see [`crate::fixed_charges`]) join the invoice before
/// coupons and taxes.
#[allow(clippy::too_many_arguments)]
pub fn generate_subscription_invoice_with_progressive(
    invoice: &mut Invoice,
    plan: &Plan,
    subscription: &Subscription,
    dates: &dyn FeeDates,
    proration: Proration,
    charges: &[ChargeInput],
    taxes: &[Tax],
    coupons: &[CouponApplication],
    progressive: &[ProgressiveBillingInvoice],
    fixed_charge_fees: Vec<Fee>,
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

    // Usage charge fees: one per filter and pricing group (`init_fees`), the
    // minimum-commitment true-up over all of them on the default bucket's fee.
    for input in charges {
        if !input.charge.invoiceable {
            continue;
        }
        let usage = input.usage();
        let charge_fees = charge_fees_all(&usage, subscription, currency, Some(invoice.id))?;
        let true_up = charge_fees
            .iter()
            .find(|f| f.charge_filter_id.is_none())
            .and_then(|parent| {
                charge_true_up_fee(parent, &charge_fees, input.charge, subscription, dates, currency)
            });
        let parent_id = true_up.as_ref().map(|_| {
            charge_fees
                .iter()
                .find(|f| f.charge_filter_id.is_none())
                .map(|f| f.id)
                .unwrap_or_default()
        });
        fees.extend(retain_used(&usage, charge_fees, parent_id));
        fees.extend(true_up);
    }

    // Fixed charges (seats, platform units) of this invoice.
    fees.extend(fixed_charge_fees.into_iter().map(|f| Fee { invoice_id: Some(invoice.id), ..f }));

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

/// `Fees::CreateTrueUpService` — when a charge carries a `min_amount_cents` and
/// its usage fees for the period (every filter and group) are below the
/// (prorated) minimum, a true-up fee on `fee` (the default bucket's) bills the
/// shortfall. The minimum is prorated over the days the subscription was active
/// in the charges period.
fn charge_true_up_fee(
    fee: &Fee,
    charge_fees: &[Fee],
    charge: &Charge,
    subscription: &Subscription,
    dates: &dyn FeeDates,
    currency: Currency,
) -> Option<Fee> {
    if charge.min_amount_cents <= 0 || charge.pay_in_advance {
        return None;
    }
    let from = dates.charges_from_date();
    let to = dates.charges_to_date();
    let started = subscription.started_at.map(|d| d.date_naive()).unwrap_or(from);
    let effective_from = if started > from { started } else { from };
    let days_to_bill = (to - effective_from).num_days() + 1;
    if days_to_bill <= 0 {
        return None;
    }
    let used_amount_cents: i64 = charge_fees.iter().map(|f| f.amount_cents).sum();
    let used_precise_amount_cents: rust_decimal::Decimal =
        charge_fees.iter().map(|f| f.precise_amount_cents).sum();
    let tu = charge_true_up(
        charge.min_amount_cents,
        dates.charges_duration_in_days(),
        days_to_bill,
        used_amount_cents,
        used_precise_amount_cents,
        currency,
    )?;
    if tu.amount_cents <= 0 {
        return None;
    }
    let mut out = fee.clone();
    out.id = uuid::Uuid::new_v4();
    out.amount_cents = tu.amount_cents;
    out.precise_amount_cents = tu.precise_amount_cents;
    out.unit_amount_cents = tu.unit_amount_cents;
    out.precise_unit_amount = tu.precise_unit_amount;
    out.units = rust_decimal::Decimal::ONE;
    out.total_aggregated_units = None;
    out.events_count = None;
    out.taxes_amount_cents = 0;
    out.taxes_precise_amount_cents = rust_decimal::Decimal::ZERO;
    out.applied_taxes = Vec::new();
    out.amount_details = serde_json::json!({});
    out.charge_filter_id = None;
    out.grouped_by = serde_json::json!({});
    out.properties = serde_json::json!({
        "true_up_parent_fee_id": fee.id,
        "min_amount_cents": charge.min_amount_cents,
        "charges_from_date": effective_from.to_string(),
        "charges_to_date": to.to_string(),
    });
    Some(out)
}

/// Convenience: build a [`Currency`] for a plan.
pub fn plan_currency(plan: &Plan) -> Currency {
    plan.currency()
}
