use rust_decimal::Decimal;

use super::{ChargeModelCalculator, PricingStructure};
use crate::aggregation::AggregationResult;
use crate::json::prop_decimal;

/// `ChargeModels::StandardService` — a flat per-unit price.
pub struct StandardService;

impl ChargeModelCalculator for StandardService {
    /// `units * BigDecimal(properties["amount"])`.
    fn compute_amount(&self, ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        agg.aggregation * prop_decimal(&ps.properties, "amount")
    }

    /// `compute_amount / (full_units_number || units)`, or 0 when no units.
    fn unit_amount(&self, ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        let total_units = agg.full_units_number.unwrap_or(agg.aggregation);
        if total_units.is_zero() {
            return Decimal::ZERO;
        }
        self.compute_amount(ps, agg) / total_units
    }
}
