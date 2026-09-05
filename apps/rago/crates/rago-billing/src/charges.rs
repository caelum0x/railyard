//! `Charges::ApplyPayInAdvanceChargeModelService` — price a single
//! pay-in-advance event as the *delta* it introduces to the running charge
//! amount.
//!
//! For a persisted event: `amount = cost(all events) - cost(all but this
//! event)`. For a not-yet-persisted estimate: `amount = cost(all events +
//! this) - cost(all events)`. Amounts are returned in integer cents.

use rago_core::money::{round_half_up, Currency};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

use crate::aggregation::AggregationResult;
use crate::charge_models::{self, PricingStructure};

/// Result of pricing one pay-in-advance event.
#[derive(Debug, Clone)]
pub struct PayInAdvanceResult {
    pub units: Decimal,
    pub count: i64,
    /// Amount in integer cents.
    pub amount_cents: Decimal,
    pub precise_amount: Decimal,
    pub unit_amount: Decimal,
}

/// Apply the pay-in-advance charge model. `ps` carries the (per-filter)
/// properties; `agg` is the aggregation for the whole period including the
/// event, with `pay_in_advance_aggregation` set to the event's contribution.
pub fn apply_pay_in_advance(ps: &PricingStructure, agg: &AggregationResult) -> PayInAdvanceResult {
    let currency: Currency = ps.currency;
    let with_persisted = agg.pay_in_advance_event_persisted;
    let pia = agg.pay_in_advance_aggregation.unwrap_or(Decimal::ZERO);

    let amount_from_aggregation = charge_models::compute_in_advance(ps, agg).amount;

    let amount = if with_persisted {
        // cost(all) - cost(all excluding this event)
        let mut without = agg.clone();
        without.aggregation -= pia;
        without.count -= 1;
        let ps_excl = ps.with_properties(merge_flag(&ps.properties, "exclude_event"));
        let excl = charge_models::compute_in_advance(&ps_excl, &without).amount;
        amount_from_aggregation - excl
    } else {
        // cost(all including this non-persisted event) - cost(all)
        let mut with = agg.clone();
        with.aggregation += pia;
        with.count += 1;
        let ps_incl = ps.with_properties(merge_flag(&ps.properties, "include_event_value"));
        let incl = charge_models::compute_in_advance(&ps_incl, &with).amount;
        incl - amount_from_aggregation
    };

    let rounded_amount = round_half_up(amount, currency.exponent());
    let subunit = currency.subunit_to_unit_decimal();
    let amount_cents = rounded_amount * subunit;

    let units = compute_units(agg);
    let unit_amount = if rounded_amount.is_zero() || units.is_zero() {
        Decimal::ZERO
    } else {
        rounded_amount / units
    };

    PayInAdvanceResult {
        units,
        count: 1,
        amount_cents,
        precise_amount: amount * subunit,
        unit_amount,
    }
}

/// `#compute_units`.
fn compute_units(agg: &AggregationResult) -> Decimal {
    if display_applied_units_for_zero_invoice(agg) {
        let ua = agg.units_applied.unwrap_or(Decimal::ZERO);
        return if ua.is_sign_negative() {
            Decimal::ZERO
        } else {
            ua
        };
    }
    agg.pay_in_advance_aggregation.unwrap_or(Decimal::ZERO)
}

/// `#display_applied_units_for_zero_invoice?`.
fn display_applied_units_for_zero_invoice(agg: &AggregationResult) -> bool {
    match (
        agg.current_aggregation,
        agg.max_aggregation,
        agg.units_applied,
    ) {
        (Some(current), Some(max), Some(_)) => current <= max,
        _ => false,
    }
}

/// Merge a boolean charge-model flag into the JSON properties.
fn merge_flag(properties: &serde_json::Value, key: &str) -> serde_json::Value {
    let mut props = properties.clone();
    if let serde_json::Value::Object(map) = &mut props {
        map.insert(key.to_string(), serde_json::Value::Bool(true));
    }
    props
}

impl PayInAdvanceResult {
    /// Amount in cents as `i64`.
    pub fn amount_cents_i64(&self) -> i64 {
        self.amount_cents.to_i64().unwrap_or(0)
    }
}
