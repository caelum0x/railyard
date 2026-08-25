//! Money is integer microdollars (µ$), identical to Mint's on-the-wire unit.
//!
//! Mint settles everything in `i64` microdollars; the game store never invents
//! its own currency — a pack price is just a µ$ amount reserved and captured on
//! the same rail as every other Railyard product.

/// One US dollar in microdollars. `$1.00 == 1_000_000 µ$`.
pub const MICROS_PER_DOLLAR: i64 = 1_000_000;

/// Convert whole dollars + cents to microdollars, e.g. `dollars(2, 99)` = $2.99.
///
/// `cents` is taken modulo 100 for the fractional part; the dollar part carries
/// the sign. Used only to author the static catalog in readable units.
pub const fn dollars(dollars: i64, cents: i64) -> i64 {
    dollars * MICROS_PER_DOLLAR + (cents % 100) * (MICROS_PER_DOLLAR / 100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dollars_and_cents_to_micros() {
        assert_eq!(dollars(0, 99), 990_000);
        assert_eq!(dollars(2, 99), 2_990_000);
        assert_eq!(dollars(9, 99), 9_990_000);
        assert_eq!(dollars(1, 0), MICROS_PER_DOLLAR);
    }
}
