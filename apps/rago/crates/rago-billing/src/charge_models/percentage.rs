use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::{json, Value};

use super::{ChargeModelCalculator, PricingStructure};
use crate::aggregation::AggregationResult;
use crate::json::{prop_decimal, prop_i64};

/// `ChargeModels::PercentageService` — a percentage of the transacted value
/// plus an optional per-event fixed fee, with free-unit/free-event allowances
/// and optional per-transaction min/max.
///
/// Note on `should_apply_min_max?`: Lago gates it on `License.premium?`. There
/// is no license concept in the port, so min/max applies whenever the
/// properties are present. This is documented as an intentional deviation; the
/// percentage/fixed path is otherwise a faithful port.
pub struct PercentageService;

impl PercentageService {
    fn rate(ps: &PricingStructure) -> Decimal {
        prop_decimal(&ps.properties, "rate")
    }
    fn fixed_amount(ps: &PricingStructure) -> Decimal {
        prop_decimal(&ps.properties, "fixed_amount")
    }
    fn free_units_per_total_aggregation(ps: &PricingStructure) -> Decimal {
        prop_decimal(&ps.properties, "free_units_per_total_aggregation")
    }
    fn free_units_per_events(ps: &PricingStructure) -> i64 {
        prop_i64(&ps.properties, "free_units_per_events")
    }
    fn last_running_total(agg: &AggregationResult) -> Decimal {
        agg.running_total.last().copied().unwrap_or(Decimal::ZERO)
    }

    /// `PercentageService#free_units_value`.
    fn free_units_value(ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        let last = Self::last_running_total(agg);
        if last.is_zero() {
            return Decimal::ZERO;
        }
        let fupe = Self::free_units_per_events(ps);
        let rt_count = agg.running_total.len() as i64;
        if fupe > 0 && fupe < rt_count {
            return agg.running_total[(fupe - 1) as usize];
        }
        let fupta = Self::free_units_per_total_aggregation(ps);
        if fupta.is_zero() {
            return last;
        }
        if last <= fupta {
            return last;
        }
        fupta
    }

    /// `PercentageService#free_units_count` — `[a, b].excluding(0).min || 0`.
    fn free_units_count(ps: &PricingStructure, agg: &AggregationResult) -> i64 {
        let fupe = Self::free_units_per_events(ps);
        let fupta = Self::free_units_per_total_aggregation(ps);
        let below = agg.running_total.iter().filter(|e| **e < fupta).count() as i64;
        [fupe, below]
            .into_iter()
            .filter(|v| *v != 0)
            .min()
            .unwrap_or(0)
    }

    /// `PercentageService#compute_percentage_amount`.
    fn compute_percentage_amount(ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        let free_units_value = Self::free_units_value(ps, agg);
        if free_units_value > agg.aggregation {
            return Decimal::ZERO;
        }
        (agg.aggregation - free_units_value) * Self::rate(ps) / dec!(100)
    }

    /// `PercentageService#compute_fixed_amount`.
    fn compute_fixed_amount(ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        if agg.aggregation.is_zero() {
            return Decimal::ZERO;
        }
        let free_units_count = Self::free_units_count(ps, agg);
        if free_units_count >= agg.count {
            return Decimal::ZERO;
        }
        Decimal::from(agg.count - free_units_count) * Self::fixed_amount(ps)
    }

    fn per_transaction_min(ps: &PricingStructure) -> Option<Decimal> {
        ps.properties
            .get("per_transaction_min_amount")
            .filter(|v| !v.is_null())
            .map(|_| prop_decimal(&ps.properties, "per_transaction_min_amount"))
    }
    fn per_transaction_max(ps: &PricingStructure) -> Option<Decimal> {
        ps.properties
            .get("per_transaction_max_amount")
            .filter(|v| !v.is_null())
            .map(|_| prop_decimal(&ps.properties, "per_transaction_max_amount"))
    }
    fn should_apply_min_max(ps: &PricingStructure) -> bool {
        Self::per_transaction_min(ps).is_some() || Self::per_transaction_max(ps).is_some()
    }

    fn apply_min_max(ps: &PricingStructure, amount: Decimal) -> Decimal {
        if let Some(min) = Self::per_transaction_min(ps) {
            if amount < min {
                return min;
            }
        }
        if let Some(max) = Self::per_transaction_max(ps) {
            if amount > max {
                return max;
            }
        }
        amount
    }

    /// `PercentageService#compute_amount_with_transaction_min_max`.
    fn compute_amount_with_transaction_min_max(
        ps: &PricingStructure,
        agg: &AggregationResult,
    ) -> Decimal {
        let mut remaining_free_events = Self::free_units_per_events(ps);
        let mut remaining_free_amount = Self::free_units_per_total_aggregation(ps);
        let rate = Self::rate(ps);
        let fixed_amount = Self::fixed_amount(ps);

        let mut total = Decimal::ZERO;
        for &event_value in &agg.per_event_values {
            let mut value = event_value;

            if remaining_free_events > 0 || remaining_free_amount > Decimal::ZERO {
                remaining_free_events -= 1;

                if remaining_free_amount <= Decimal::ZERO {
                    // `next 0` — the fold accumulator is reset to zero.
                    total = Decimal::ZERO;
                    continue;
                }

                if remaining_free_amount > value {
                    remaining_free_amount -= value;
                    total = Decimal::ZERO;
                    continue;
                } else {
                    value -= remaining_free_amount;
                    remaining_free_amount = Decimal::ZERO;
                    remaining_free_events = 0;
                }
            }

            let mut event_amount = (value * rate) / dec!(100);
            event_amount += fixed_amount;
            event_amount = Self::apply_min_max(ps, event_amount);
            total += event_amount;
        }
        total
    }

    fn min_max_adjustment_total_amount(ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        if !Self::should_apply_min_max(ps) {
            return Decimal::ZERO;
        }
        Self::compute_amount_with_transaction_min_max(ps, agg)
            - Self::compute_percentage_amount(ps, agg)
            - Self::compute_fixed_amount(ps, agg)
    }
}

impl ChargeModelCalculator for PercentageService {
    /// `PercentageService#compute_amount`.
    fn compute_amount(&self, ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        if Self::should_apply_min_max(ps) {
            return Self::compute_amount_with_transaction_min_max(ps, agg);
        }
        Self::compute_percentage_amount(ps, agg) + Self::compute_fixed_amount(ps, agg)
    }

    fn unit_amount(&self, ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        let total_units = agg.full_units_number.unwrap_or(agg.aggregation);
        if total_units.is_zero() {
            return Decimal::ZERO;
        }
        self.compute_amount(ps, agg) / total_units
    }

    /// `PercentageService#amount_details`.
    fn amount_details(&self, ps: &PricingStructure, agg: &AggregationResult) -> Value {
        let free_units_value = Self::free_units_value(ps, agg);
        let mut paid_units = agg.aggregation - free_units_value;
        if paid_units.is_sign_negative() {
            paid_units = Decimal::ZERO;
        }
        let free_units_count = Self::free_units_count(ps, agg);
        let free_events = if agg.count >= free_units_count {
            free_units_count
        } else {
            agg.count
        };
        let paid_events = agg.count - free_events;

        json!({
            "units": agg.aggregation.to_string(),
            "free_units": free_units_value.to_string(),
            "free_events": free_events,
            "paid_units": paid_units.to_string(),
            "rate": Self::rate(ps).to_string(),
            "per_unit_total_amount": Self::compute_percentage_amount(ps, agg).to_string(),
            "paid_events": paid_events,
            "fixed_fee_unit_amount": if paid_events > 0 { Self::fixed_amount(ps).to_string() } else { Decimal::ZERO.to_string() },
            "fixed_fee_total_amount": Self::compute_fixed_amount(ps, agg).to_string(),
            "min_max_adjustment_total_amount": Self::min_max_adjustment_total_amount(ps, agg).to_string(),
        })
    }
}
