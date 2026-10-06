//! `Charges::EstimateInstant::{Standard,Percentage}Service` — the amount one
//! event costs on a pay-in-advance charge, without reading stored events
//! (Lago `POST /events/estimate_instant_fees`).

use rago_core::enums::ChargeModel;
use rust_decimal::Decimal;
use serde_json::Value;

fn dec(props: &Value, key: &str) -> Option<Decimal> {
    match props.get(key)? {
        Value::String(s) if !s.trim().is_empty() => s.trim().parse().ok(),
        Value::Number(n) => n.to_string().parse().ok(),
        _ => None,
    }
}

/// The amount (main currency unit) of `units` under `properties`, or `None`
/// for a charge model that cannot be estimated instantly (only standard and
/// percentage can). Negative units cost nothing.
pub fn instant_amount(model: ChargeModel, properties: &Value, units: Decimal) -> Option<Decimal> {
    if units.is_sign_negative() {
        return Some(Decimal::ZERO);
    }
    match model {
        ChargeModel::Standard => Some(units * dec(properties, "amount").unwrap_or_default()),
        ChargeModel::Percentage => {
            let rate = dec(properties, "rate").unwrap_or_default();
            let mut amount = units * rate / Decimal::from(100) + dec(properties, "fixed_amount").unwrap_or_default();
            if let Some(min) = dec(properties, "per_transaction_min_amount") {
                amount = amount.max(min);
            }
            if let Some(max) = dec(properties, "per_transaction_max_amount") {
                amount = amount.min(max);
            }
            Some(amount)
        }
        _ => None,
    }
}

/// Whether a charge model can be estimated instantly.
pub fn is_instant(model: ChargeModel) -> bool {
    matches!(model, ChargeModel::Standard | ChargeModel::Percentage)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;
    use serde_json::json;

    #[test]
    fn standard_is_units_times_amount() {
        assert_eq!(instant_amount(ChargeModel::Standard, &json!({ "amount": "0.5" }), dec!(12)), Some(dec!(6.0)));
        assert_eq!(instant_amount(ChargeModel::Standard, &json!({ "amount": "2" }), dec!(-3)), Some(Decimal::ZERO));
    }

    #[test]
    fn percentage_adds_the_fixed_amount_and_clamps_per_transaction() {
        let p = json!({ "rate": "2", "fixed_amount": "0.3", "per_transaction_min_amount": "1", "per_transaction_max_amount": "5" });
        assert_eq!(instant_amount(ChargeModel::Percentage, &p, dec!(100)), Some(dec!(2.3)));
        assert_eq!(instant_amount(ChargeModel::Percentage, &p, dec!(10)), Some(dec!(1)), "the minimum");
        assert_eq!(instant_amount(ChargeModel::Percentage, &p, dec!(1000)), Some(dec!(5)), "the maximum");
    }

    #[test]
    fn other_models_are_not_instant() {
        assert_eq!(instant_amount(ChargeModel::Graduated, &json!({}), dec!(1)), None);
        assert!(!is_instant(ChargeModel::Package));
    }
}
