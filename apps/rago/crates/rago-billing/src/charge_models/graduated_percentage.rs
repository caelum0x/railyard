use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::{json, Value};

use super::range_graduated::Range;
use super::{ChargeModelCalculator, PricingStructure};
use crate::aggregation::AggregationResult;

/// `ChargeModels::GraduatedPercentageService` — tiered percentage pricing:
/// each tier applies a `rate` (%) to the units within it, plus a flat amount.
pub struct GraduatedPercentageService;

/// One graduated-percentage tier detail (`RangeGraduatedPercentageService`).
struct RangeDetail {
    from_value: Decimal,
    to_value: Option<Decimal>,
    flat_unit_amount: Decimal,
    rate: Decimal,
    units: Decimal,
    per_unit_total_amount: Decimal,
    total_with_flat_amount: Decimal,
}

impl GraduatedPercentageService {
    fn ranges(ps: &PricingStructure) -> Vec<Range> {
        ps.properties
            .get("graduated_percentage_ranges")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().map(Range::parse).collect())
            .unwrap_or_default()
    }

    /// `RangeGraduatedPercentageService#units`.
    fn range_units(range: &Range, total_units: Decimal) -> Decimal {
        if let Some(to) = range.to_value {
            if total_units >= to {
                let base = if range.from_value.is_zero() {
                    Decimal::ONE
                } else {
                    range.from_value
                };
                return to - base + Decimal::ONE;
            }
        }
        if range.from_value.is_zero() {
            return total_units;
        }
        total_units - range.from_value + Decimal::ONE
    }

    fn range_detail(range: &Range, total_units: Decimal) -> RangeDetail {
        let units = Self::range_units(range, total_units);
        let per_unit_total_amount = units * range.rate / dec!(100);
        let flat_unit_amount = range.flat_amount;
        RangeDetail {
            from_value: range.from_value,
            to_value: range.to_value,
            flat_unit_amount,
            rate: range.rate,
            units,
            per_unit_total_amount,
            total_with_flat_amount: per_unit_total_amount + flat_unit_amount,
        }
    }

    fn range_details(ps: &PricingStructure, units: Decimal) -> Vec<RangeDetail> {
        let ranges = Self::ranges(ps);
        let mut out = Vec::new();
        for range in &ranges {
            out.push(Self::range_detail(range, units));
            match range.to_value {
                None => break,
                Some(to) if to >= units => break,
                _ => {}
            }
        }
        out
    }
}

impl ChargeModelCalculator for GraduatedPercentageService {
    /// Sum of each tier's `total_with_flat_amount`.
    fn compute_amount(&self, ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        Self::range_details(ps, agg.aggregation)
            .iter()
            .map(|d| d.total_with_flat_amount)
            .sum()
    }

    fn unit_amount(&self, ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        let total_units = agg.full_units_number.unwrap_or(agg.aggregation);
        if total_units.is_zero() {
            return Decimal::ZERO;
        }
        self.compute_amount(ps, agg) / total_units
    }

    fn amount_details(&self, ps: &PricingStructure, agg: &AggregationResult) -> Value {
        let details: Vec<Value> = Self::range_details(ps, agg.aggregation)
            .iter()
            .map(|d| {
                json!({
                    "from_value": d.from_value.to_string().parse::<i64>().unwrap_or_default(),
                    "to_value": d.to_value.map(|t| t.to_string().parse::<i64>().unwrap_or_default()),
                    "flat_unit_amount": d.flat_unit_amount.to_string(),
                    "rate": d.rate.to_string(),
                    "units": d.units.to_string(),
                    "per_unit_total_amount": d.per_unit_total_amount.to_string(),
                    "total_with_flat_amount": d.total_with_flat_amount.to_string(),
                })
            })
            .collect();
        json!({ "graduated_percentage_ranges": details })
    }
}
