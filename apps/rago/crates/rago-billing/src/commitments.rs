//! Minimum commitments and true-up fees.
//!
//! Two related "spend at least X" mechanisms, both faithful ports of Lago:
//!
//! * **Charge minimum** (`Fees::CreateTrueUpService`) — a `min_amount_cents` on
//!   a single charge. If the charge's usage fee for the period is below the
//!   (prorated) minimum, a *true-up* fee tops it up to the minimum.
//!
//! * **Plan minimum commitment** (`Commitments::Minimum::CalculateTrueUpFeeService`
//!   + `Commitments::CalculateAmountService`) — a minimum spend across *all* the
//!   subscription's fees for the period. If the period's fees total less than
//!   the (prorated) commitment, a commitment true-up fee bills the shortfall.
//!
//! The commitment amount is prorated by
//! `Commitments::CalculateProratedCoefficientService`:
//! `coefficient = billed_days / period_days`, and
//! `commitment_amount_cents = round(commitment.amount_cents * coefficient)`.

use rago_core::money::{round_to_integer, Currency};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;

/// The result of a charge/commitment true-up computation: the extra amount to
/// bill (already `None` when nothing is owed).
#[derive(Debug, Clone, PartialEq)]
pub struct TrueUp {
    pub amount_cents: i64,
    pub precise_amount_cents: Decimal,
    pub unit_amount_cents: i64,
    pub precise_unit_amount: Decimal,
}

/// `Fees::CreateTrueUpService` — the charge-level `min_amount_cents` top-up.
///
/// `prorated_min = min_amount_cents / charges_duration * days_to_bill`. If the
/// already-billed usage (`used_amount_cents`) meets the prorated minimum, no
/// true-up is due; otherwise the true-up bills the difference.
///
/// * `used_amount_cents` — the charge fee's rounded `amount_cents`.
/// * `used_precise_amount_cents` — the charge fee's `precise_amount_cents`.
/// * `days_to_bill` — days between charges_from and charges_to (= charges_duration
///   for a full period).
pub fn charge_true_up(
    min_amount_cents: i64,
    charges_duration: i64,
    days_to_bill: i64,
    used_amount_cents: i64,
    used_precise_amount_cents: Decimal,
    currency: Currency,
) -> Option<TrueUp> {
    if charges_duration <= 0 || min_amount_cents <= 0 {
        return None;
    }
    let prorated_min = Decimal::from(min_amount_cents) / Decimal::from(charges_duration)
        * Decimal::from(days_to_bill);

    // return if used_amount_cents >= prorated_min_amount_cents
    if Decimal::from(used_amount_cents) >= prorated_min {
        return None;
    }

    let precise_amount_cents = prorated_min - used_precise_amount_cents;
    let amount_cents = round_to_integer(prorated_min - Decimal::from(used_amount_cents))
        .to_i64()
        .unwrap_or(0);
    let precise_unit_amount = precise_amount_cents / currency.subunit_to_unit_decimal();

    Some(TrueUp {
        amount_cents,
        precise_amount_cents,
        unit_amount_cents: amount_cents,
        precise_unit_amount,
    })
}

/// `Commitments::CalculateProratedCoefficientService#calculate_proration_coefficient`
/// — `billed_days / period_days`.
pub fn proration_coefficient(billed_days: i64, period_days: i64) -> Decimal {
    if period_days <= 0 {
        return Decimal::ZERO;
    }
    Decimal::from(billed_days) / Decimal::from(period_days)
}

/// `Commitments::CalculateAmountService#commitment_amount_cents` —
/// `round(commitment_amount_cents * proration_coefficient)`. A full period
/// (coefficient 1) returns the commitment amount unchanged.
pub fn commitment_amount_cents(
    commitment_amount_cents: i64,
    billed_days: i64,
    period_days: i64,
) -> i64 {
    if commitment_amount_cents == 0 {
        return 0;
    }
    let coeff = proration_coefficient(billed_days, period_days);
    round_to_integer(Decimal::from(commitment_amount_cents) * coeff)
        .to_i64()
        .unwrap_or(0)
}

/// `Commitments::Minimum::CalculateTrueUpFeeService#{amount_cents,precise_amount_cents}`
/// — the plan minimum-commitment true-up: `commitment - fees_total` when the
/// period's fees fall short of the (prorated) commitment, else zero.
///
/// `commitment_amount_cents` is the already-prorated commitment (from
/// [`commitment_amount_cents`]). Returns `None` when nothing is owed.
pub fn minimum_commitment_true_up(
    commitment_amount_cents: i64,
    fees_total_amount_cents: i64,
    fees_total_precise_amount_cents: Decimal,
) -> Option<TrueUp> {
    if commitment_amount_cents == 0 || fees_total_amount_cents >= commitment_amount_cents {
        return None;
    }
    let commitment = Decimal::from(commitment_amount_cents);
    let amount_cents = commitment_amount_cents - fees_total_amount_cents;
    let precise_amount_cents = if fees_total_precise_amount_cents >= commitment {
        Decimal::ZERO
    } else {
        commitment - fees_total_precise_amount_cents
    };
    Some(TrueUp {
        amount_cents,
        precise_amount_cents,
        unit_amount_cents: amount_cents,
        precise_unit_amount: Decimal::ZERO,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_macros::dec;

    fn usd() -> Currency {
        Currency::new("USD")
    }

    // ---- Lago spec: fees/create_true_up_service_spec.rb ----

    #[test]
    fn charge_true_up_full_period() {
        // min 1000, charges_duration 31, full period (31 days), used 700.
        // prorated_min = 1000; true_up = 300, precise 300, unit 300, precise_unit 3.0.
        let tu = charge_true_up(1000, 31, 31, 700, dec!(700.0), usd()).unwrap();
        assert_eq!(tu.amount_cents, 300);
        assert_eq!(tu.precise_amount_cents, dec!(300.0));
        assert_eq!(tu.unit_amount_cents, 300);
        assert_eq!(tu.precise_unit_amount, dec!(3));
    }

    #[test]
    fn charge_true_up_none_when_used_exceeds_min() {
        // Fee amount 1500 >= min 1000 → no true-up.
        assert!(charge_true_up(1000, 31, 31, 1500, dec!(1500.0), usd()).is_none());
    }

    #[test]
    fn charge_true_up_prorated_period() {
        // min 1000, charges_duration 31, days_to_bill 15, used 200.
        // prorated_min = 1000/31*15 = 483.8709677419355; amount = round(283.87) = 284.
        let tu = charge_true_up(1000, 31, 15, 200, dec!(200.0), usd()).unwrap();
        assert_eq!(tu.amount_cents, 284);
        // Lago asserts precise 283.8709677419355; compare at 10 dp (rust_decimal
        // and Ruby BigDecimal keep different high-precision tails, but the money
        // number — amount_cents 284 — is identical).
        use rust_decimal::RoundingStrategy;
        assert_eq!(
            tu.precise_amount_cents
                .round_dp_with_strategy(10, RoundingStrategy::MidpointAwayFromZero),
            dec!(283.8709677419)
        );
    }

    // ---- Lago spec: commitments/calculate_amount_service_spec.rb ----

    #[test]
    fn commitment_amount_full_period_is_unchanged() {
        assert_eq!(commitment_amount_cents(3000, 7, 7), 3000);
    }

    #[test]
    fn commitment_amount_prorated() {
        // amount 3000, billed 6 days of a 7-day period → 3000*6/7 = 2571.43 → 2571.
        assert_eq!(commitment_amount_cents(3000, 6, 7), 2571);
    }

    #[test]
    fn commitment_amount_zero_commitment() {
        assert_eq!(commitment_amount_cents(0, 6, 7), 0);
    }

    // ---- Lago spec: commitments/minimum/*/calculate_true_up_fee_service ----

    #[test]
    fn minimum_commitment_true_up_bills_shortfall() {
        // commitment 3000, fees total 2000 → true up 1000.
        let tu = minimum_commitment_true_up(3000, 2000, dec!(2000)).unwrap();
        assert_eq!(tu.amount_cents, 1000);
        assert_eq!(tu.precise_amount_cents, dec!(1000));
    }

    #[test]
    fn minimum_commitment_true_up_none_when_met() {
        assert!(minimum_commitment_true_up(3000, 3000, dec!(3000)).is_none());
        assert!(minimum_commitment_true_up(3000, 3500, dec!(3500)).is_none());
    }

    #[test]
    fn minimum_commitment_true_up_precise() {
        // fees precise below commitment → precise diff kept exact.
        let tu = minimum_commitment_true_up(3000, 2000, dec!(1999.5)).unwrap();
        assert_eq!(tu.amount_cents, 1000);
        assert_eq!(tu.precise_amount_cents, dec!(1000.5));
    }
}
