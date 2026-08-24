//! Integer microdollar money math (ADR-0001).
//!
//! `1 AIT = 1_000_000 µ$`. All arithmetic is checked and integer-only — there
//! are deliberately **no** `Add`/`Sub` trait impls (callers must take the
//! checked path) and **no** `f32`/`f64` conversions anywhere in this module.
//! Display formatting lives in [`Micros::format_usd`], off the arithmetic path.

use serde::{Deserialize, Serialize};

use crate::error::MintError;

/// Microdollars per AIT. `1 AIT = 1 USD = 1_000_000 µ$`.
pub const MICROS_PER_AIT: i64 = 1_000_000;

/// A signed integer microdollar amount. `Copy`, ordered, hashable, and
/// serialized as a **bare `i64`** (never a float).
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

    /// Checked addition. Returns [`MintError::Overflow`] instead of wrapping.
    pub fn checked_add(self, other: Micros) -> Result<Micros, MintError> {
        self.0
            .checked_add(other.0)
            .map(Micros)
            .ok_or(MintError::Overflow)
    }

    /// Checked subtraction. Returns [`MintError::Overflow`] instead of wrapping.
    pub fn checked_sub(self, other: Micros) -> Result<Micros, MintError> {
        self.0
            .checked_sub(other.0)
            .map(Micros)
            .ok_or(MintError::Overflow)
    }

    /// Checked negation (`0 - self`). Returns [`MintError::Overflow`] at
    /// `i64::MIN`.
    pub fn checked_neg(self) -> Result<Micros, MintError> {
        Micros::zero().checked_sub(self)
    }

    /// `true` iff strictly greater than zero.
    pub const fn is_positive(self) -> bool {
        self.0 > 0
    }

    /// `true` iff strictly less than zero.
    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }

    /// `true` iff exactly zero.
    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    /// Human-readable USD rendering (`$22.99`). **Display only** — never used
    /// on the arithmetic path. Truncates toward zero at 2 decimal places.
    pub fn format_usd(self) -> String {
        let neg = self.0 < 0;
        let abs = (self.0 as i128).unsigned_abs();
        let per = MICROS_PER_AIT as u128;
        let dollars = abs / per;
        let cents = (abs % per) / 10_000;
        format!("{}${}.{:02}", if neg { "-" } else { "" }, dollars, cents)
    }
}

impl std::fmt::Debug for Micros {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Micros({})", self.0)
    }
}

/// A validated transfer amount: strictly `> 0`. Constructing one is the only
/// gate, so a non-positive transfer amount is unrepresentable past this
/// boundary. Serializes as a bare `i64`.
#[derive(Copy, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct TransferAmount(Micros);

impl TransferAmount {
    /// Validate that `m > 0`. Returns [`MintError::InvalidAmount`] otherwise.
    pub fn new(m: Micros) -> Result<Self, MintError> {
        if m.is_positive() {
            Ok(TransferAmount(m))
        } else {
            Err(MintError::InvalidAmount)
        }
    }

    /// The underlying (guaranteed-positive) [`Micros`].
    pub const fn micros(self) -> Micros {
        self.0
    }

    /// The underlying raw positive microdollar count.
    pub const fn get(self) -> i64 {
        self.0.get()
    }
}

impl std::fmt::Debug for TransferAmount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "TransferAmount({})", self.0 .0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_add_overflow_is_reported_not_wrapped() {
        let max = Micros::new(i64::MAX);
        assert_eq!(max.checked_add(Micros::new(1)), Err(MintError::Overflow));
        assert_eq!(max.checked_add(Micros::zero()), Ok(Micros::new(i64::MAX)));
    }

    #[test]
    fn checked_sub_underflow_is_reported_not_wrapped() {
        let min = Micros::new(i64::MIN);
        assert_eq!(min.checked_sub(Micros::new(1)), Err(MintError::Overflow));
        assert_eq!(min.checked_neg(), Err(MintError::Overflow));
    }

    #[test]
    fn exact_integer_arithmetic() {
        let a = Micros::new(2_990_000); // $2.99
        let b = Micros::new(20_000_000); // $20.00
        assert_eq!(a.checked_add(b), Ok(Micros::new(22_990_000)));
        assert_eq!(b.checked_sub(a), Ok(Micros::new(17_010_000)));
    }

    #[test]
    fn transfer_amount_rejects_non_positive() {
        assert_eq!(
            TransferAmount::new(Micros::zero()),
            Err(MintError::InvalidAmount)
        );
        assert_eq!(
            TransferAmount::new(Micros::new(-1)),
            Err(MintError::InvalidAmount)
        );
        assert_eq!(TransferAmount::new(Micros::new(1)).unwrap().get(), 1);
    }

    #[test]
    fn serializes_as_bare_i64_never_float() {
        let json = serde_json::to_string(&Micros::new(22_990_000)).unwrap();
        assert_eq!(json, "22990000");
        assert!(!json.contains('.'));
        let back: Micros = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Micros::new(22_990_000));
    }

    #[test]
    fn format_usd_display_helper() {
        assert_eq!(Micros::new(22_990_000).format_usd(), "$22.99");
        assert_eq!(Micros::new(0).format_usd(), "$0.00");
        assert_eq!(Micros::new(-1_500_000).format_usd(), "-$1.50");
    }
}
