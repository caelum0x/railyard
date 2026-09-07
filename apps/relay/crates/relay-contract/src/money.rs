//! Exact integer-microdollar public money representation.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use ts_rs::TS;

pub const MICROS_PER_USD: i64 = 1_000_000;

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, TS,
)]
#[serde(transparent)]
#[ts(type = "number")]
pub struct Microdollars(i64);

impl Microdollars {
    pub const ZERO: Self = Self(0);

    pub const fn new(value: i64) -> Self {
        Self(value)
    }

    pub fn non_negative(value: i64) -> Result<Self, MoneyError> {
        if value < 0 {
            return Err(MoneyError::Negative(value));
        }
        Ok(Self(value))
    }

    pub const fn get(self) -> i64 {
        self.0
    }

    pub fn checked_add(self, other: Self) -> Result<Self, MoneyError> {
        self.0
            .checked_add(other.0)
            .map(Self)
            .ok_or(MoneyError::Overflow)
    }

    pub fn checked_sub(self, other: Self) -> Result<Self, MoneyError> {
        self.0
            .checked_sub(other.0)
            .map(Self)
            .ok_or(MoneyError::Overflow)
    }

    /// Parse decimal USD exactly, rounding half-up at six decimal places.
    pub fn from_usd_decimal(raw: &str) -> Result<Self, MoneyError> {
        let raw = raw.trim();
        if raw.is_empty() {
            return Err(MoneyError::InvalidDecimal);
        }
        let raw = raw.strip_prefix('+').unwrap_or(raw);
        if raw.starts_with('-') {
            return Err(MoneyError::InvalidDecimal);
        }
        let (mantissa, exponent) =
            raw.split_once(['e', 'E'])
                .map_or(Ok((raw, 0_i32)), |(mantissa, exponent)| {
                    exponent
                        .parse::<i32>()
                        .map(|exponent| (mantissa, exponent))
                        .map_err(|_| MoneyError::InvalidDecimal)
                })?;
        let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        if whole.is_empty()
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(MoneyError::InvalidDecimal);
        }
        let digits = format!("{whole}{fraction}")
            .parse::<i128>()
            .map_err(|_| MoneyError::Overflow)?;
        let fraction_len = i32::try_from(fraction.len()).map_err(|_| MoneyError::Overflow)?;
        let scale = 6_i32
            .checked_add(exponent)
            .and_then(|value| value.checked_sub(fraction_len))
            .ok_or(MoneyError::Overflow)?;
        let micros = if scale >= 0 {
            let factor = checked_pow10(u32::try_from(scale).map_err(|_| MoneyError::Overflow)?)?;
            digits.checked_mul(factor).ok_or(MoneyError::Overflow)?
        } else {
            let divisor = checked_pow10(scale.unsigned_abs())?;
            digits
                .checked_add(divisor / 2)
                .ok_or(MoneyError::Overflow)?
                / divisor
        };
        i64::try_from(micros)
            .map(Self)
            .map_err(|_| MoneyError::Overflow)
    }
}

fn checked_pow10(exponent: u32) -> Result<i128, MoneyError> {
    10_i128.checked_pow(exponent).ok_or(MoneyError::Overflow)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MoneyError {
    #[error("money amount cannot be negative: {0}")]
    Negative(i64),
    #[error("money arithmetic overflow")]
    Overflow,
    #[error("invalid decimal USD amount")]
    InvalidDecimal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_conversion_is_exact_and_half_up() {
        assert_eq!(
            Microdollars::from_usd_decimal("1.2345674"),
            Ok(Microdollars::new(1_234_567))
        );
        assert_eq!(
            Microdollars::from_usd_decimal("1.2345675"),
            Ok(Microdollars::new(1_234_568))
        );
    }

    #[test]
    fn invalid_or_negative_decimal_is_rejected() {
        for value in ["", "-1", "NaN", "1e", ".5", "1.2.3"] {
            assert!(Microdollars::from_usd_decimal(value).is_err(), "{value}");
        }
    }

    #[test]
    fn scientific_json_notation_is_exact() {
        assert_eq!(
            Microdollars::from_usd_decimal("6e-6"),
            Ok(Microdollars::new(6))
        );
        assert_eq!(
            Microdollars::from_usd_decimal("1.25e2"),
            Ok(Microdollars::new(125_000_000))
        );
    }
}
