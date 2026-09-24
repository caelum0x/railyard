//! # rago-billing
//!
//! The billing engine — faithful Rust port of Lago's `app/services/*` pricing
//! pipeline. Given usage events, a billable metric, and a charge, it produces
//! fees and assembles them (with taxes and coupons) into invoice totals:
//!
//! ```text
//! events ──▶ aggregation ──▶ charge model ──▶ fee (cents + taxes) ──▶ invoice
//! ```
//!
//! * [`aggregation`] — `BillableMetrics::Aggregations::{Sum,Count}Service`.
//! * [`charge_models`] — `ChargeModels::*` (standard, graduated, package,
//!   volume, percentage).
//! * [`fees`] — `Fees::ChargeService#init_fee` + `Fees::ApplyTaxesService`.
//! * [`invoices`] — coupons + `Invoices::ComputeAmountsFromFees` +
//!   `Invoices::ApplyTaxesService`.
//!
//! All money math is exact ([`rust_decimal::Decimal`]); amounts convert to
//! integer cents only at fee-build time, matching Lago's Ruby `BigDecimal`
//! semantics (ties away from zero).

pub mod aggregation;
pub mod charge_models;
pub mod charges;
pub mod commitments;
pub mod credit_notes;
pub mod dates;
pub mod downgrade;
pub mod dunning;
pub mod error;
pub mod events;
pub mod expression;
pub mod fees;
pub mod generate;
pub mod invoices;
mod json;
pub mod prepaid;
pub mod progressive_billing;
pub mod proration;
pub mod subscription_fee;
pub mod subscriptions;
pub mod trials;
pub mod usage_thresholds;
pub mod wallet_realtime;
pub mod wallets;

use rago_core::models::{BillableMetric, Charge, Event, Fee, Subscription};
use rago_core::money::Currency;
use rust_decimal::Decimal;
use uuid::Uuid;

use crate::charge_models::PricingStructure;
use crate::error::Result;

/// End-to-end charge → fee for one charge in arrears: aggregate the events,
/// apply the charge model, and build the fee (without taxes).
///
/// This is the spine of `Fees::ChargeService#call` for the common
/// (non-filtered, in-arrears) path.
pub fn charge_to_fee(
    metric: &BillableMetric,
    charge: &Charge,
    subscription: &Subscription,
    events: &[Event],
    currency: Currency,
    invoice_id: Option<Uuid>,
) -> Result<Fee> {
    charge_to_fee_in_period(metric, charge, subscription, events, currency, invoice_id, None)
}

/// [`charge_to_fee`] with the billing period the events belong to, so
/// time-weighted aggregations (`weighted_sum_agg`) weight over the real period.
pub fn charge_to_fee_in_period(
    metric: &BillableMetric,
    charge: &Charge,
    subscription: &Subscription,
    events: &[Event],
    currency: Currency,
    invoice_id: Option<Uuid>,
    period: Option<&aggregation::AggregationPeriod>,
) -> Result<Fee> {
    // Free-unit options come from the charge properties (percentage model).
    let free_units_per_events = crate::json::prop_i64(&charge.properties, "free_units_per_events");
    let free_units_per_total_aggregation =
        crate::json::prop_decimal(&charge.properties, "free_units_per_total_aggregation");

    let agg = aggregation::aggregate_in_period(
        metric,
        events,
        free_units_per_events,
        free_units_per_total_aggregation,
        period,
    )?;

    let ps = PricingStructure::from_charge(charge, currency);
    let cmr = charge_models::compute(&ps, &agg)?;

    Ok(fees::build_charge_fee(
        cmr,
        charge,
        subscription,
        invoice_id,
        currency,
    ))
}

/// Convenience re-export for callers that only want the aggregated units.
pub use aggregation::{aggregate, aggregate_in_period, AggregationPeriod, AggregationResult};

/// A raw fractional unit value, for tests/examples.
pub type Units = Decimal;
