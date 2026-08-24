//! Microdollar money type — the single source of truth for the money path.
//!
//! ADR-0001 invariant: money is **integer microdollars** (`1 USD = 1_000_000`
//! micros) end to end. There is NO float anywhere in the money path. All
//! arithmetic is checked so ledger deltas reconcile exactly and never wrap.
//! Human-facing `$x.xx` formatting happens ONLY at the display edge.

use serde::{Deserialize, Serialize};

use crate::error::RelayError;

/// Integer microdollars per USD (`1_000_000`).
pub const MICROS_PER_USD: i64 = 1_000_000;

/// Hard ceiling ($1,000,000) so a malformed request can never mint an absurd
/// amount.
const MAX_USD: i64 = 1_000_000;
const MAX_MICROS: i64 = MAX_USD * MICROS_PER_USD;

/// A signed integer-microdollar amount.
///
/// Signed so ledger deltas can be negative (Reserve / Debit). Addition and
/// subtraction go through `checked_*` and return a `Result` — overflow is a
/// reconciliation bug, never a silent wrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Micros(i64);

impl Micros {
    /// Zero microdollars.
    pub const ZERO: Micros = Micros(0);

    /// Construct from a raw microdollar count.
    pub const fn new(micros: i64) -> Self {
        Micros(micros)
    }

    /// The raw microdollar count.
    pub const fn get(self) -> i64 {
        self.0
    }

    /// `true` if the amount is negative (a hold or debit delta).
    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }

    /// Checked addition. Returns `BadRequest` on overflow so reconciliation is
    /// guaranteed exact and never wraps.
    pub fn checked_add(self, rhs: Micros) -> Result<Micros, RelayError> {
        self.0
            .checked_add(rhs.0)
            .map(Micros)
            .ok_or_else(|| RelayError::BadRequest("money overflow on add".into()))
    }

    /// Checked subtraction. Returns `BadRequest` on overflow.
    pub fn checked_sub(self, rhs: Micros) -> Result<Micros, RelayError> {
        self.0
            .checked_sub(rhs.0)
            .map(Micros)
            .ok_or_else(|| RelayError::BadRequest("money overflow on sub".into()))
    }

    /// Checked negation (flips a Reserve hold into its Release, or vice versa).
    pub fn checked_neg(self) -> Result<Micros, RelayError> {
        self.0
            .checked_neg()
            .map(Micros)
            .ok_or_else(|| RelayError::BadRequest("money overflow on neg".into()))
    }

    /// serde default helper for `Usage::cost_micros`.
    pub(crate) fn zero() -> Micros {
        Micros::ZERO
    }

    /// Render as a display-edge `$x.xx` string. Display ONLY — never feed the
    /// result back into the money path.
    pub fn format_usd(self) -> String {
        let negative = self.0 < 0;
        let abs = (self.0 as i128).unsigned_abs();
        let micros_per_cent = (MICROS_PER_USD / 100) as u128; // 10_000
        let cents_total = (abs + micros_per_cent / 2) / micros_per_cent;
        let dollars = cents_total / 100;
        let cents = cents_total % 100;
        let sign = if negative { "-" } else { "" };
        format!("{sign}${dollars}.{cents:02}")
    }
}

/// Parse a USD amount string (e.g. OpenRouter pricing like `"0.0000015"`) into
/// exact integer microdollars. Integer-scaling with HALF_UP rounding at the
/// 6th decimal — no float in the path.
///
/// * empty / whitespace -> `Ok(Micros::ZERO)`
/// * negative -> `BadRequest`
/// * non-numeric / scientific / NaN / inf -> `BadRequest`
/// * above the $1,000,000 ceiling -> `BadRequest`
pub fn usd_to_micros(value: &str) -> Result<Micros, RelayError> {
    let s = value.trim();
    if s.is_empty() {
        return Ok(Micros::ZERO);
    }

    let (negative, body) = match s.as_bytes()[0] {
        b'+' => (false, &s[1..]),
        b'-' => (true, &s[1..]),
        _ => (false, s),
    };
    if negative {
        return Err(bad(value));
    }
    if body.is_empty() {
        return Err(bad(value));
    }

    let mut parts = body.splitn(2, '.');
    let int_part = parts.next().unwrap_or("");
    let frac_part = parts.next().unwrap_or("");
    if !int_part.bytes().all(|b| b.is_ascii_digit())
        || !frac_part.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(bad(value));
    }
    if int_part.is_empty() && frac_part.is_empty() {
        return Err(bad(value));
    }

    let dollars: i64 = if int_part.is_empty() {
        0
    } else {
        int_part.parse::<i64>().map_err(|_| bad(value))?
    };
    let mut micros = dollars
        .checked_mul(MICROS_PER_USD)
        .ok_or_else(|| over_max(value))?;

    let frac_bytes = frac_part.as_bytes();
    let mut scaled: i64 = 0;
    for i in 0..6 {
        let d = frac_bytes.get(i).map(|b| (b - b'0') as i64).unwrap_or(0);
        scaled = scaled * 10 + d;
    }
    let round_up = frac_bytes.get(6).map(|b| *b >= b'5').unwrap_or(false);
    if round_up {
        scaled += 1;
    }
    micros = micros.checked_add(scaled).ok_or_else(|| over_max(value))?;

    if micros > MAX_MICROS {
        return Err(over_max(value));
    }
    Ok(Micros(micros))
}

fn bad(value: &str) -> RelayError {
    RelayError::BadRequest(format!("invalid USD amount: {value:?}"))
}

fn over_max(value: &str) -> RelayError {
    RelayError::BadRequest(format!("USD amount exceeds maximum: {value:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn micros_per_usd_is_one_million() {
        assert_eq!(MICROS_PER_USD, 1_000_000);
    }

    #[test]
    fn checked_add_sums_exactly() {
        let a = Micros::new(1_500_000);
        let b = Micros::new(2_500_000);
        assert_eq!(a.checked_add(b).unwrap(), Micros::new(4_000_000));
    }

    #[test]
    fn checked_sub_can_go_negative_for_holds() {
        let bal = Micros::new(1_000_000);
        let hold = Micros::new(3_000_000);
        assert_eq!(bal.checked_sub(hold).unwrap(), Micros::new(-2_000_000));
    }

    #[test]
    fn checked_add_overflow_is_error_not_wrap() {
        let big = Micros::new(i64::MAX);
        assert!(big.checked_add(Micros::new(1)).is_err());
    }

    #[test]
    fn checked_sub_overflow_is_error_not_wrap() {
        let low = Micros::new(i64::MIN);
        assert!(low.checked_sub(Micros::new(1)).is_err());
    }

    #[test]
    fn checked_neg_flips_sign() {
        assert_eq!(
            Micros::new(-500_000).checked_neg().unwrap(),
            Micros::new(500_000)
        );
    }

    #[test]
    fn reservation_reconciles_to_zero() {
        let estimate = Micros::new(750_000);
        let reserve = estimate.checked_neg().unwrap();
        let net = reserve.checked_add(estimate).unwrap();
        assert_eq!(net, Micros::ZERO);
    }

    #[test]
    fn usd_to_micros_matches_python_cases() {
        assert_eq!(usd_to_micros("20.00").unwrap(), Micros::new(20_000_000));
        assert_eq!(usd_to_micros("0").unwrap(), Micros::ZERO);
        assert_eq!(usd_to_micros("").unwrap(), Micros::ZERO);
        assert_eq!(usd_to_micros("   ").unwrap(), Micros::ZERO);
        assert_eq!(usd_to_micros("2.5").unwrap(), Micros::new(2_500_000));
        assert_eq!(usd_to_micros("1.234567").unwrap(), Micros::new(1_234_567));
        assert_eq!(usd_to_micros("0.0000005").unwrap(), Micros::new(1));
        assert_eq!(usd_to_micros("5").unwrap(), Micros::new(5_000_000));
        assert_eq!(usd_to_micros(".5").unwrap(), Micros::new(500_000));
        assert_eq!(usd_to_micros("5.").unwrap(), Micros::new(5_000_000));
        assert_eq!(usd_to_micros("0.0000015").unwrap(), Micros::new(2));
    }

    #[test]
    fn usd_to_micros_rejects_bad() {
        for bad in ["abc", "-1", "nan", "inf", "1e400", "1.2.3", "$5", "1,000"].iter() {
            assert!(
                usd_to_micros(bad).is_err(),
                "expected {bad:?} to be rejected"
            );
        }
    }

    #[test]
    fn usd_to_micros_rejects_over_max() {
        assert!(usd_to_micros("2000000").is_err());
        assert!(usd_to_micros("1000000.01").is_err());
        assert_eq!(usd_to_micros("1000000").unwrap(), Micros::new(MAX_MICROS));
    }

    #[test]
    fn format_usd_two_decimals_with_prefix() {
        assert_eq!(Micros::new(21_000_000).format_usd(), "$21.00");
        assert_eq!(Micros::ZERO.format_usd(), "$0.00");
        assert_eq!(Micros::new(2_500_000).format_usd(), "$2.50");
        assert_eq!(Micros::new(1_235_000).format_usd(), "$1.24");
        assert_eq!(Micros::new(1_234_000).format_usd(), "$1.23");
        assert_eq!(Micros::new(-2_500_000).format_usd(), "-$2.50");
    }

    #[test]
    fn micros_serde_is_transparent_integer() {
        let v = serde_json::to_string(&Micros::new(1_234_567)).unwrap();
        assert_eq!(v, "1234567");
        let back: Micros = serde_json::from_str("1234567").unwrap();
        assert_eq!(back, Micros::new(1_234_567));
    }
}
