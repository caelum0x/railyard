//! Integer money conversions from provider wire formats into microdollars.
//!
//! `1 USD = 1_000_000 µ$`, `1 cent = 10_000 µ$`. All conversions are checked and
//! integer-only — a decimal string is parsed digit-by-digit (never through
//! `f64`) so `"22.99"` becomes exactly `22_990_000 µ$`.

use serde_json::Value;

/// Microdollars per whole unit of currency.
pub const MICROS_PER_UNIT: i64 = 1_000_000;

/// Convert a JSON amount field — either a decimal string (`"4.99"`) or a JSON
/// number (`4.99`) — into microdollars WITHOUT floating point. A JSON number is
/// rendered to its canonical decimal string first (never through `f64`).
pub fn json_dollars_to_micros(v: &Value) -> Option<i64> {
    match v {
        Value::String(s) => decimal_to_micros(s),
        Value::Number(n) => decimal_to_micros(&n.to_string()),
        _ => None,
    }
}

/// Convert a minor-unit (cent) integer amount into microdollars.
/// `cents × 10_000`. Returns `None` on `i64` overflow.
pub fn cents_to_micros(cents: i64) -> Option<i64> {
    cents.checked_mul(10_000)
}

/// Convert a decimal currency string (e.g. `"22.99"`, `"0.500000"`) into
/// microdollars WITHOUT any floating point. Excess fractional digits beyond 6
/// are truncated toward zero; missing digits are right-padded. Returns `None`
/// on malformed input or overflow.
pub fn decimal_to_micros(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (neg, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let mut parts = s.splitn(2, '.');
    let int_part = parts.next().unwrap_or("");
    let frac_part = parts.next().unwrap_or("");
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    if !int_part.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    if !frac_part.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let mut frac = frac_part.to_string();
    if frac.len() > 6 {
        frac.truncate(6);
    } else {
        while frac.len() < 6 {
            frac.push('0');
        }
    }
    let int_v: i64 = if int_part.is_empty() {
        0
    } else {
        int_part.parse().ok()?
    };
    let frac_v: i64 = frac.parse().ok()?;
    let micros = int_v.checked_mul(MICROS_PER_UNIT)?.checked_add(frac_v)?;
    Some(if neg { -micros } else { micros })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cents_convert_exactly() {
        assert_eq!(cents_to_micros(2299), Some(22_990_000));
        assert_eq!(cents_to_micros(0), Some(0));
        assert_eq!(cents_to_micros(i64::MAX), None);
    }

    #[test]
    fn decimals_convert_without_float() {
        assert_eq!(decimal_to_micros("22.99"), Some(22_990_000));
        assert_eq!(decimal_to_micros("22"), Some(22_000_000));
        assert_eq!(decimal_to_micros("0.5"), Some(500_000));
        assert_eq!(decimal_to_micros(".5"), Some(500_000));
        assert_eq!(decimal_to_micros("1.2345678"), Some(1_234_567)); // truncated
        assert_eq!(decimal_to_micros("-1.50"), Some(-1_500_000));
    }

    #[test]
    fn malformed_decimals_are_rejected() {
        assert_eq!(decimal_to_micros(""), None);
        assert_eq!(decimal_to_micros("abc"), None);
        assert_eq!(decimal_to_micros("1.2.3"), None);
        assert_eq!(decimal_to_micros("1,50"), None);
    }
}
