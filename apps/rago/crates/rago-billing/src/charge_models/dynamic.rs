use rust_decimal::Decimal;

use super::{ChargeModelCalculator, PricingStructure};
use crate::aggregation::AggregationResult;

/// `ChargeModels::DynamicService` — the price is carried by the events
/// themselves (`precise_total_amount_cents`), converted from cents to the main
/// currency unit.
pub struct DynamicService;

impl ChargeModelCalculator for DynamicService {
    /// `precise_total_amount_cents / currency.subunit_to_unit`.
    fn compute_amount(&self, ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        let total_units = agg.full_units_number.unwrap_or(agg.aggregation);
        if total_units.is_zero() {
            return Decimal::ZERO;
        }
        let amount_cents = agg.precise_total_amount_cents.unwrap_or(Decimal::ZERO);
        amount_cents / ps.currency.subunit_to_unit_decimal()
    }

    fn unit_amount(&self, ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        let total_units = agg.full_units_number.unwrap_or(agg.aggregation);
        if total_units.is_zero() {
            return Decimal::ZERO;
        }
        self.compute_amount(ps, agg) / total_units
    }
}
