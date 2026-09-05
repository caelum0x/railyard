use rust_decimal::Decimal;

use super::{ChargeModelCalculator, PricingStructure};
use crate::aggregation::AggregationResult;

/// `ChargeModels::CustomService` — the amount comes from a customer-defined
/// aggregation (`custom_aggregation[:amount]`).
pub struct CustomService;

impl ChargeModelCalculator for CustomService {
    fn compute_amount(&self, _ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        agg.custom_amount.unwrap_or(Decimal::ZERO)
    }

    fn unit_amount(&self, ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        let total_units = agg.full_units_number.unwrap_or(agg.aggregation);
        if total_units.is_zero() {
            return Decimal::ZERO;
        }
        self.compute_amount(ps, agg) / total_units
    }
}
