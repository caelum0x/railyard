//! `ChargeModels::AmountDetails::RangeGraduatedService` — the per-tier math for
//! the graduated charge model.

use rust_decimal::Decimal;
use serde_json::{json, Value};

use crate::json::to_decimal;

/// One graduated tier's billing detail.
#[derive(Debug, Clone)]
pub struct RangeDetail {
    pub from_value: Decimal,
    pub to_value: Option<Decimal>,
    pub flat_unit_amount: Decimal,
    pub per_unit_amount: Decimal,
    pub units: Decimal,
    pub per_unit_total_amount: Decimal,
    pub total_with_flat_amount: Decimal,
}

impl RangeDetail {
    /// `RangeGraduatedService#to_json`-shaped hash for `amount_details`.
    pub fn to_json(&self) -> Value {
        json!({
            "from_value": decimal_to_json(self.from_value),
            "to_value": self.to_value.map(decimal_to_json),
            "flat_unit_amount": self.flat_unit_amount.to_string(),
            "per_unit_amount": self.per_unit_amount.to_string(),
            "units": self.units.to_string(),
            "per_unit_total_amount": self.per_unit_total_amount.to_string(),
            "total_with_flat_amount": self.total_with_flat_amount.to_string(),
        })
    }
}

fn decimal_to_json(d: Decimal) -> Value {
    // from_value/to_value are integers in Lago's stored ranges.
    if d.fract().is_zero() {
        json!(d.trunc().to_string().parse::<i64>().unwrap_or_default())
    } else {
        json!(d.to_string())
    }
}

/// A parsed graduated/volume/graduated-percentage range from `properties`.
#[derive(Debug, Clone)]
pub struct Range {
    pub from_value: Decimal,
    pub to_value: Option<Decimal>,
    pub per_unit_amount: Decimal,
    pub flat_amount: Decimal,
    /// Percentage rate (graduated-percentage ranges only).
    pub rate: Decimal,
}

impl Range {
    /// Parse `{from_value, to_value, per_unit_amount, flat_amount, rate}` the
    /// way Ruby's `with_indifferent_access` reads them.
    pub fn parse(value: &Value) -> Self {
        let from_value = to_decimal(value.get("from_value"));
        let to_value = match value.get("to_value") {
            Some(Value::Null) | None => None,
            other => Some(to_decimal(other)),
        };
        Self {
            from_value,
            to_value,
            per_unit_amount: to_decimal(value.get("per_unit_amount")),
            flat_amount: to_decimal(value.get("flat_amount")),
            rate: to_decimal(value.get("rate")),
        }
    }
}

/// `RangeGraduatedService#call` — compute the billing detail for one tier
/// given the total units and whether the ranges are adjacent.
pub fn compute(range: &Range, total_units: Decimal, adjacent_model: bool) -> RangeDetail {
    let units = range_units(range, total_units, adjacent_model);
    let per_unit_amount = if units.is_zero() {
        Decimal::ZERO
    } else {
        range.per_unit_amount
    };
    let per_unit_total_amount = units * per_unit_amount;
    let flat_unit_amount = range.flat_amount;
    let total_with_flat_amount = per_unit_total_amount + flat_unit_amount;

    RangeDetail {
        from_value: range.from_value,
        to_value: range.to_value,
        flat_unit_amount,
        per_unit_amount,
        units,
        per_unit_total_amount,
        total_with_flat_amount,
    }
}

/// `RangeGraduatedService#units` — how many units fall inside this tier.
fn range_units(range: &Range, total_units: Decimal, adjacent_model: bool) -> Decimal {
    let effective_total = match range.to_value {
        Some(to) if total_units >= to => to,
        _ => total_units,
    };

    if range.from_value.is_zero() {
        return effective_total;
    }

    let diff = effective_total - range.from_value;
    if adjacent_model {
        diff
    } else {
        diff + Decimal::ONE
    }
}
