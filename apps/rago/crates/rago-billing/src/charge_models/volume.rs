use rust_decimal::Decimal;
use serde_json::{json, Value};

use super::range_graduated::Range;
use super::{ChargeModelCalculator, PricingStructure};
use crate::aggregation::AggregationResult;

/// `ChargeModels::VolumeService` — the *whole* usage is priced at the single
/// tier it falls into (unlike graduated, which prices tier-by-tier).
pub struct VolumeService;

impl VolumeService {
    fn ranges(ps: &PricingStructure) -> Vec<Range> {
        let mut ranges: Vec<Range> = ps
            .properties
            .get("volume_ranges")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().map(Range::parse).collect())
            .unwrap_or_default();
        ranges.sort_by(|a, b| a.from_value.cmp(&b.from_value));
        ranges
    }

    /// `VolumeService#number_of_units`.
    fn number_of_units(ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        if ps.prorated {
            if let Some(full) = agg.full_units_number {
                return full;
            }
        }
        agg.aggregation
    }

    /// `VolumeService#matching_range` — first tier containing `number_of_units`.
    fn matching_range(ps: &PricingStructure, agg: &AggregationResult) -> Option<Range> {
        let number_of_units = Self::number_of_units(ps, agg);
        let ceil_units = number_of_units.ceil();
        Self::ranges(ps).into_iter().find(|r| {
            r.from_value <= ceil_units && r.to_value.map_or(true, |to| number_of_units <= to)
        })
    }

    fn per_unit_total_amount(ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        match Self::matching_range(ps, agg) {
            Some(r) => agg.aggregation * r.per_unit_amount,
            None => Decimal::ZERO,
        }
    }

    fn flat_unit_amount(ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        Self::matching_range(ps, agg)
            .map(|r| r.flat_amount)
            .unwrap_or(Decimal::ZERO)
    }
}

impl ChargeModelCalculator for VolumeService {
    /// `VolumeService#compute_amount` = per_unit_total + flat.
    fn compute_amount(&self, ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        Self::per_unit_total_amount(ps, agg) + Self::flat_unit_amount(ps, agg)
    }

    fn unit_amount(&self, ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        let number_of_units = Self::number_of_units(ps, agg);
        if number_of_units.is_zero() {
            return Decimal::ZERO;
        }
        self.compute_amount(ps, agg) / number_of_units
    }

    /// `VolumeService#amount_details`.
    fn amount_details(&self, ps: &PricingStructure, agg: &AggregationResult) -> Value {
        let number_of_units = Self::number_of_units(ps, agg);
        let per_unit_total_amount = Self::per_unit_total_amount(ps, agg);
        let per_unit_amount = if number_of_units.is_zero() {
            Decimal::ZERO.to_string()
        } else {
            (per_unit_total_amount / number_of_units).to_string()
        };
        json!({
            "flat_unit_amount": Self::flat_unit_amount(ps, agg).to_string(),
            "per_unit_amount": per_unit_amount,
            "per_unit_total_amount": per_unit_total_amount.to_string(),
        })
    }
}
