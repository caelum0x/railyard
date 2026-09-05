//! Helpers for reading Lago's JSON `properties` the way Ruby's
//! `BigDecimal(properties["x"])` / `properties["x"].to_i` coercions do.

use rust_decimal::Decimal;
use serde_json::Value;
use std::str::FromStr;

/// `BigDecimal(value.to_s)` — coerce a JSON string/number to [`Decimal`],
/// defaulting to zero for `nil`/absent (Ruby `BigDecimal(x || 0)`).
pub fn to_decimal(value: Option<&Value>) -> Decimal {
    match value {
        Some(Value::String(s)) => Decimal::from_str(s.trim()).unwrap_or(Decimal::ZERO),
        Some(Value::Number(n)) => {
            // `BigDecimal(value.to_s)` — parse the number's exact decimal token
            // (as Ruby does), NOT its lossy f64 value. Going through `as_f64`
            // would inject binary-float error into decimal tier boundaries
            // (e.g. JSON `0.1` -> 0.1000000000000000055…), diverging from Lago.
            Decimal::from_str(&n.to_string())
                .or_else(|_| n.as_f64().and_then(Decimal::from_f64_retain).ok_or(()))
                .unwrap_or(Decimal::ZERO)
        }
        _ => Decimal::ZERO,
    }
}

/// `BigDecimal(properties["key"])` — read a required decimal property.
pub fn prop_decimal(props: &Value, key: &str) -> Decimal {
    to_decimal(props.get(key))
}

/// `properties["key"].to_i` — read an integer property, defaulting to 0.
pub fn prop_i64(props: &Value, key: &str) -> i64 {
    match props.get(key) {
        Some(Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(Value::String(s)) => s.trim().parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

/// Whether a truthy `exclude_event` flag is present in `properties`.
pub fn prop_bool(props: &Value, key: &str) -> bool {
    matches!(props.get(key), Some(Value::Bool(true)))
}
