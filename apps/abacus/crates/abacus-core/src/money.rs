//! Integer-µ$ money math (ADR-0001). NO floats on the money path — ever.
//!
//! [`Micros`] is a signed `i64` microdollar amount serialized as a bare integer.
//! `1 USD = 1_000_000 µ$`. All arithmetic is checked; overflow is an error, never
//! a wrap. Pricing (per-token cost tables) rounds the estimated ceiling UP so a
//! looping agent can never be under-reserved by fractional-micro truncation.

use serde::{Deserialize, Serialize};

use crate::error::AbacusError;

/// Microdollars per dollar. `1 USD = 1_000_000 µ$`.
pub const MICROS_PER_USD: i64 = 1_000_000;

/// A signed integer microdollar amount, serialized as a bare `i64` (never float).
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Micros(i64);

impl Micros {
    /// Construct from a raw microdollar count.
    pub const fn new(v: i64) -> Self {
        Micros(v)
    }
    /// The additive identity (`0 µ$`).
    pub const fn zero() -> Self {
        Micros(0)
    }
    /// The raw microdollar count.
    pub const fn get(self) -> i64 {
        self.0
    }
    /// `true` iff strictly greater than zero.
    pub const fn is_positive(self) -> bool {
        self.0 > 0
    }
    /// `true` iff strictly less than zero.
    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }
    /// Checked addition; [`AbacusError::Overflow`] instead of wrapping.
    pub fn checked_add(self, other: Micros) -> Result<Micros, AbacusError> {
        self.0
            .checked_add(other.0)
            .map(Micros)
            .ok_or(AbacusError::Overflow)
    }
    /// Checked subtraction; [`AbacusError::Overflow`] instead of wrapping.
    pub fn checked_sub(self, other: Micros) -> Result<Micros, AbacusError> {
        self.0
            .checked_sub(other.0)
            .map(Micros)
            .ok_or(AbacusError::Overflow)
    }
    /// Human-readable USD rendering (`$22.99`), display-only, truncating.
    pub fn format_usd(self) -> String {
        let neg = self.0 < 0;
        let abs = (self.0 as i128).unsigned_abs();
        let per = MICROS_PER_USD as u128;
        format!(
            "{}${}.{:02}",
            if neg { "-" } else { "" },
            abs / per,
            (abs % per) / 10_000
        )
    }
}

impl std::fmt::Debug for Micros {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Micros({})", self.0)
    }
}

/// Ceiling multiply-and-divide for a non-negative `value * num / den`, computed
/// in `i128` then narrowed back to `i64`. Used by pricing so a priced ceiling is
/// never rounded *down* below the true cost.
pub fn mul_div_ceil(value: i64, num: i64, den: i64) -> Result<i64, AbacusError> {
    if den <= 0 {
        return Err(AbacusError::InvalidAmount);
    }
    if value < 0 || num < 0 {
        return Err(AbacusError::InvalidAmount);
    }
    let n = value as i128 * num as i128;
    let d = den as i128;
    let q = (n + (d - 1)) / d;
    i64::try_from(q).map_err(|_| AbacusError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_as_bare_i64() {
        assert_eq!(
            serde_json::to_string(&Micros::new(22_990_000)).unwrap(),
            "22990000"
        );
    }

    #[test]
    fn checked_add_detects_overflow() {
        assert_eq!(
            Micros::new(i64::MAX).checked_add(Micros::new(1)),
            Err(AbacusError::Overflow)
        );
        assert_eq!(
            Micros::new(2).checked_add(Micros::new(3)).unwrap(),
            Micros::new(5)
        );
    }

    #[test]
    fn mul_div_ceil_rounds_up() {
        // 1 token * 1 µ$ / 1000 => 0.001 -> ceil => 1 µ$.
        assert_eq!(mul_div_ceil(1, 1, 1000).unwrap(), 1);
        // exact division stays exact.
        assert_eq!(mul_div_ceil(1000, 3, 1000).unwrap(), 3);
    }

    #[test]
    fn mul_div_ceil_rejects_bad_inputs() {
        assert_eq!(mul_div_ceil(1, 1, 0), Err(AbacusError::InvalidAmount));
        assert_eq!(mul_div_ceil(-1, 1, 1), Err(AbacusError::InvalidAmount));
    }

    #[test]
    fn format_usd_renders_dollars() {
        assert_eq!(Micros::new(1_500_000).format_usd(), "$1.50");
        assert_eq!(Micros::new(-2_990_000).format_usd(), "-$2.99");
    }
}
