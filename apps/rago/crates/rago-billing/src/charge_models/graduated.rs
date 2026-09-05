use rust_decimal::Decimal;
use serde_json::{json, Value};

use super::range_graduated::{self, Range};
use super::{ChargeModelCalculator, PricingStructure};
use crate::aggregation::AggregationResult;
use crate::json::prop_bool;

/// `ChargeModels::GraduatedService` — tiered pricing where each tier prices the
/// units that fall within it and adds an optional flat amount.
pub struct GraduatedService;

impl GraduatedService {
    fn ranges(ps: &PricingStructure) -> Vec<Range> {
        ps.properties
            .get("graduated_ranges")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().map(Range::parse).collect())
            .unwrap_or_default()
    }

    /// `GraduatedService#adjacent_ranges?` — true when each tier's `from_value`
    /// equals the previous tier's `to_value` (so no `+1` offset is needed).
    fn adjacent_ranges(ranges: &[Range]) -> bool {
        if ranges.len() < 2 {
            return false;
        }
        ranges.windows(2).all(|w| {
            let prev_to = w[0].to_value.unwrap_or(Decimal::ZERO);
            w[1].from_value == prev_to
        })
    }

    /// `GraduatedService#amount_details[:graduated_ranges]` — the per-tier
    /// breakdown, stopping once the tier covering `units` is reached.
    fn range_details(ps: &PricingStructure, units: Decimal) -> Vec<range_graduated::RangeDetail> {
        let ranges = Self::ranges(ps);
        let adjacent = Self::adjacent_ranges(&ranges);
        let mut out = Vec::new();
        for range in &ranges {
            out.push(range_graduated::compute(range, units, adjacent));
            match range.to_value {
                None => break,
                Some(to) if to >= units => break,
                _ => {}
            }
        }
        out
    }
}

impl ChargeModelCalculator for GraduatedService {
    /// `GraduatedService#compute_amount` — sum of each tier's
    /// `total_with_flat_amount`. Zero when there are no units on a pay-in-advance
    /// `exclude_event` pass.
    fn compute_amount(&self, ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        let units = agg.aggregation;
        if units.is_zero() && prop_bool(&ps.properties, "exclude_event") {
            return Decimal::ZERO;
        }
        Self::range_details(ps, units)
            .iter()
            .map(|d| d.total_with_flat_amount)
            .sum()
    }

    /// `GraduatedService#unit_amount`.
    fn unit_amount(&self, ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        let total_units = agg.full_units_number.unwrap_or(agg.aggregation);
        if total_units.is_zero() {
            return Decimal::ZERO;
        }
        self.compute_amount(ps, agg) / total_units
    }

    /// `GraduatedService#amount_details`.
    fn amount_details(&self, ps: &PricingStructure, agg: &AggregationResult) -> Value {
        let details: Vec<Value> = Self::range_details(ps, agg.aggregation)
            .iter()
            .map(|d| d.to_json())
            .collect();
        json!({ "graduated_ranges": details })
    }
}
