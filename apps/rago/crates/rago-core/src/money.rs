//! Money & currency value types.
//!
//! Faithful port of the parts of the Ruby `Money`/`Money::Currency` gem that
//! Lago relies on for billing math. Amounts are stored in the database as
//! integer **cents** (`amount_cents bigint`) plus a currency code, together
//! with high-precision `precise_amount_cents numeric` columns. Charge-model
//! math is done in the currency's *main* unit as an exact decimal and only
//! converted to cents at fee-build time — mirroring Lago exactly.
//!
//! Ruby `BigDecimal#round(n)` defaults to ROUND_HALF_UP (ties away from zero).
//! rust_decimal defaults to banker's rounding, so every place Lago rounds we
//! use [`RoundingStrategy::MidpointAwayFromZero`] to match the Ruby numbers.

use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};

/// An ISO-4217 currency, carrying the two pieces of information the billing
/// math needs: the number of decimal places (`exponent`) and the number of
/// subunits in one main unit (`subunit_to_unit`, e.g. 100 cents per dollar).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Currency {
    code: [u8; 3],
    exponent: u32,
    subunit_to_unit: i64,
}

impl Currency {
    /// Build a currency from its 3-letter ISO code (case-insensitive).
    ///
    /// Mirrors `Money::Currency.new(code)`. Unknown codes fall back to the
    /// 2-decimal / 100-subunit default, which is what the overwhelming
    /// majority of ISO-4217 currencies use.
    pub fn new(code: &str) -> Self {
        let upper = code.to_ascii_uppercase();
        let (exponent, subunit_to_unit) = match upper.as_str() {
            // zero-decimal currencies
            "JPY" | "KRW" | "CLP" | "VND" | "XAF" | "XOF" | "XPF" | "BIF" | "DJF" | "GNF"
            | "KMF" | "MGA" | "PYG" | "RWF" | "UGX" | "VUV" => (0, 1),
            // three-decimal currencies
            "BHD" | "IQD" | "JOD" | "KWD" | "LYD" | "OMR" | "TND" => (3, 1000),
            // everything else: 2 decimals, 100 subunits
            _ => (2, 100),
        };
        let mut bytes = [b'?'; 3];
        for (i, b) in upper.bytes().take(3).enumerate() {
            bytes[i] = b;
        }
        Self { code: bytes, exponent, subunit_to_unit }
    }

    /// ISO-4217 code, e.g. `"USD"`.
    pub fn code(&self) -> String {
        String::from_utf8_lossy(&self.code).trim_end_matches('?').to_string()
    }

    /// Number of decimal places (`Money::Currency#exponent`).
    pub fn exponent(&self) -> u32 {
        self.exponent
    }

    /// Subunits per main unit (`Money::Currency#subunit_to_unit`).
    pub fn subunit_to_unit(&self) -> i64 {
        self.subunit_to_unit
    }

    /// `subunit_to_unit` as a [`Decimal`] for exact arithmetic.
    pub fn subunit_to_unit_decimal(&self) -> Decimal {
        Decimal::from(self.subunit_to_unit)
    }
}

/// Round a decimal to a number of decimal places using Ruby's default
/// `BigDecimal#round` behaviour (ties away from zero / ROUND_HALF_UP).
pub fn round_half_up(value: Decimal, dp: u32) -> Decimal {
    value.round_dp_with_strategy(dp, RoundingStrategy::MidpointAwayFromZero)
}

/// Round a decimal to the nearest integer, half away from zero — the Ruby
/// `BigDecimal#round` / `Float#round` default, used for `*_amount_cents`.
pub fn round_to_integer(value: Decimal) -> Decimal {
    value.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    #[test]
    fn usd_is_two_decimals_hundred_subunits() {
        let usd = Currency::new("usd");
        assert_eq!(usd.code(), "USD");
        assert_eq!(usd.exponent(), 2);
        assert_eq!(usd.subunit_to_unit(), 100);
    }

    #[test]
    fn jpy_is_zero_decimal() {
        let jpy = Currency::new("JPY");
        assert_eq!(jpy.exponent(), 0);
        assert_eq!(jpy.subunit_to_unit(), 1);
    }

    #[test]
    fn half_up_matches_ruby() {
        // Ruby: BigDecimal("2.345").round(2) => 2.35 (ties away from zero)
        assert_eq!(round_half_up(dec!(2.345), 2), dec!(2.35));
        assert_eq!(round_to_integer(dec!(0.5)), dec!(1));
        assert_eq!(round_to_integer(dec!(2.5)), dec!(3));
    }
}
