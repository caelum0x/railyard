use rust_decimal::Decimal;
use serde_json::{json, Value};

use super::{ChargeModelCalculator, PricingStructure};
use crate::aggregation::AggregationResult;
use crate::json::{prop_decimal, prop_i64};

/// `ChargeModels::PackageService` — price per package of N units, with an
/// optional number of free units.
pub struct PackageService;

impl PackageService {
    fn free_units(ps: &PricingStructure) -> Decimal {
        Decimal::from(prop_i64(&ps.properties, "free_units"))
    }
    fn per_package_size(ps: &PricingStructure) -> Decimal {
        Decimal::from(prop_i64(&ps.properties, "package_size"))
    }
    fn per_package_unit_amount(ps: &PricingStructure) -> Decimal {
        prop_decimal(&ps.properties, "amount")
    }
    fn paid_units(ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        agg.aggregation - Self::free_units(ps)
    }
}

impl ChargeModelCalculator for PackageService {
    /// `PackageService#compute_amount` — packages are rounded up (ceil).
    fn compute_amount(&self, ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        let paid_units = Self::paid_units(ps, agg);
        if paid_units.is_sign_negative() {
            return Decimal::ZERO;
        }
        let size = Self::per_package_size(ps);
        if size.is_zero() {
            return Decimal::ZERO;
        }
        // paid_units.fdiv(per_package_size).ceil
        let package_count = (paid_units / size).ceil();
        package_count * Self::per_package_unit_amount(ps)
    }

    fn unit_amount(&self, ps: &PricingStructure, agg: &AggregationResult) -> Decimal {
        let paid_units = Self::paid_units(ps, agg);
        if paid_units <= Decimal::ZERO {
            return Decimal::ZERO;
        }
        self.compute_amount(ps, agg) / paid_units
    }

    /// `PackageService#amount_details`.
    fn amount_details(&self, ps: &PricingStructure, agg: &AggregationResult) -> Value {
        let free_units = Self::free_units(ps);
        let per_package_size = prop_i64(&ps.properties, "package_size");
        let per_package_unit_amount = Self::per_package_unit_amount(ps);

        if agg.aggregation.is_zero() {
            return json!({
                "free_units": "0.0",
                "paid_units": "0.0",
                "per_package_size": 0,
                "per_package_unit_amount": "0.0",
            });
        }

        let paid_units = Self::paid_units(ps, agg);
        let paid_units_str = if paid_units.is_sign_negative() {
            "0.0".to_string()
        } else {
            paid_units.to_string()
        };

        json!({
            "free_units": free_units.to_string(),
            "paid_units": paid_units_str,
            "per_package_size": per_package_size,
            "per_package_unit_amount": per_package_unit_amount.to_string(),
        })
    }
}
