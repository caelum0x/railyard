//! `ChargeModels::ProratedGraduatedService` — graduated pricing where each
//! event's units are prorated by the fraction of the period they were active.
//!
//! This is a faithful line-by-line port of the Ruby `compute_amount` loop. It
//! walks two parallel per-event arrays — full units (which decide the tier) and
//! prorated units (which decide the priced quantity) — handling tier overflow
//! when a single event spans a tier boundary.

use rust_decimal::Decimal;
use serde_json::{json, Value};

use super::range_graduated::Range;
use super::{ChargeModelCalculator, PricingStructure};
use crate::aggregation::AggregationResult;

pub struct ProratedGraduatedService;

impl ProratedGraduatedService {
    fn ranges(ps: &PricingStructure) -> Vec<Range> {
        ps.properties
            .get("graduated_ranges")
            .and_then(Value::as_array)
            .map(|arr| arr.iter().map(Range::parse).collect())
            .unwrap_or_default()
    }

    fn prorated_coefficient(prorated_value: Decimal, full_value: Decimal) -> Decimal {
        if full_value.is_zero() {
            Decimal::ZERO
        } else {
            prorated_value / full_value
        }
    }

    /// `#range(full_units, overflow, next_full_unit)`.
    fn pick_range(
        ranges: &[Range],
        full_units: Decimal,
        overflow: Decimal,
        next_full_unit: Option<Decimal>,
    ) -> &Range {
        if full_units <= Decimal::ZERO {
            return &ranges[0];
        }
        let units = if overflow.is_zero() {
            full_units
        } else if overflow.is_sign_positive() {
            full_units - overflow + Decimal::ONE
        } else {
            full_units + overflow
        };
        for (index, range) in ranges.iter().enumerate() {
            let next_positive = next_full_unit
                .map(|v| v.is_sign_positive() && !v.is_zero())
                .unwrap_or(false);
            if Some(units) == range.to_value && next_positive {
                return &ranges[index + 1];
            }
            if Some(units) == range.to_value {
                return range;
            }
            if units >= range.from_value && range.to_value.map_or(true, |to| units < to) {
                return range;
            }
        }
        &ranges[0]
    }

    /// `#calculate_overflow`.
    fn calculate_overflow(
        full_sum: Decimal,
        to_value: Option<Decimal>,
        from_value: Decimal,
    ) -> Decimal {
        match to_value {
            None => full_sum - from_value + Decimal::ONE,
            Some(to) => {
                if full_sum >= to {
                    full_sum - to
                } else {
                    full_sum - from_value + Decimal::ONE
                }
            }
        }
    }

    /// `#skip_overflow_calculation?`.
    fn skip_overflow(full_sum: Decimal, to_value: Option<Decimal>, from_value: Decimal) -> bool {
        match to_value {
            None => full_sum >= from_value - Decimal::ONE,
            Some(to) => full_sum < to && full_sum >= from_value,
        }
    }

    /// `#result_with_flat_amount`.
    fn result_with_flat_amount(
        ranges: &[Range],
        result: Decimal,
        total_full_units: Decimal,
        max_full_units: Decimal,
    ) -> Decimal {
        if total_full_units.is_sign_negative() {
            return Decimal::ZERO;
        }
        let mut result = if result.is_sign_negative() {
            Decimal::ZERO
        } else {
            result
        };
        let mut flat_amount = Decimal::ZERO;
        for range in ranges {
            flat_amount += range.flat_amount;
            if range.to_value.is_none() || max_full_units <= range.to_value.unwrap() {
                result += flat_amount;
                return result;
            }
        }
        result
    }

    fn compute(ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        let ranges = Self::ranges(ps);
        if ranges.is_empty() {
            return Decimal::ZERO;
        }
        let full_units = &agg.event_full_values;
        let prorated_units = &agg.event_prorated_values;
        let units_count = prorated_units.len();

        if agg.aggregation.is_zero() {
            return Self::result_with_flat_amount(
                &ranges,
                Decimal::ZERO,
                Decimal::ZERO,
                Decimal::ZERO,
            );
        }

        let mut index: usize = 0;
        let mut overflow = Decimal::ZERO;
        let mut full_sum = Decimal::ZERO;
        let mut max_full_sum = Decimal::ZERO;
        let mut prorated_sum = Decimal::ZERO;
        let mut result_amount = Decimal::ZERO;
        // Keep the last picked range for the post-loop application.
        let mut last_range: &Range = &ranges[0];

        while index < units_count || !overflow.is_zero() {
            let next_full_unit = full_units.get(index).copied();
            let range = Self::pick_range(&ranges, full_sum, overflow, next_full_unit);
            last_range = range;

            if !overflow.is_zero() {
                let coeff =
                    Self::prorated_coefficient(prorated_units[index - 1], full_units[index - 1]);
                prorated_sum += overflow * coeff;

                if let Some(to) = range.to_value {
                    if full_sum >= to {
                        overflow = full_sum - to;
                        prorated_sum -= overflow * coeff;
                        result_amount += prorated_sum * range.per_unit_amount;
                        prorated_sum = Decimal::ZERO;
                        continue;
                    }
                }
                overflow = Decimal::ZERO;
            }

            let Some(&prorated) = prorated_units.get(index) else {
                break;
            };
            let full = full_units.get(index).copied().unwrap_or(Decimal::ZERO);

            if prorated.is_zero() && full.is_sign_positive() && !full.is_zero() {
                index += 1;
                continue;
            }
            if prorated.is_zero() && full.is_sign_negative() && (full_sum + full).is_sign_negative()
            {
                index += 1;
                continue;
            }

            full_sum += full;
            if full_sum > max_full_sum {
                max_full_sum = full_sum;
            }
            prorated_sum += prorated;
            index += 1;

            if Self::skip_overflow(full_sum, range.to_value, range.from_value) {
                continue;
            }

            overflow = Self::calculate_overflow(full_sum, range.to_value, range.from_value);
            let coeff =
                Self::prorated_coefficient(prorated_units[index - 1], full_units[index - 1]);
            prorated_sum -= overflow * coeff;
            result_amount += prorated_sum * range.per_unit_amount;
            prorated_sum = Decimal::ZERO;
        }

        result_amount += prorated_sum * last_range.per_unit_amount;
        Self::result_with_flat_amount(&ranges, result_amount, full_sum, max_full_sum)
    }
}

impl ChargeModelCalculator for ProratedGraduatedService {
    fn compute_amount(&self, ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        Self::compute(ps, agg)
    }

    fn unit_amount(&self, ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        let total_units: Decimal = agg.event_full_values.iter().copied().sum();
        if total_units.is_zero() {
            return Decimal::ZERO;
        }
        self.compute_amount(ps, agg) / total_units
    }

    fn amount_details(&self, _ps: &PricingStructure, _agg: &AggregationResult) -> Value {
        json!({})
    }
}
