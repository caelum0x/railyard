//! `Invoices::RegenerateFromVoidedService`: the fees of a re-issued invoice.
//!
//! Without adjustments the voided invoice's fees are copied as they were.
//! With adjustments (by source fee id) a fee takes new `units` and/or
//! `unit_amount_cents`; its amount and taxes are recomputed (taxes at the
//! fee's own rate) and the invoice totals are rebuilt from the fees. A fee
//! adjusted to zero units is left out.

use rust_decimal::prelude::ToPrimitive;
use rust_decimal::{Decimal, RoundingStrategy};
use uuid::Uuid;

use rago_core::models::{Fee, Invoice};

/// One requested change to a source fee.
#[derive(Debug, Clone, PartialEq)]
pub struct FeeAdjustment {
    pub fee_id: Uuid,
    pub units: Option<Decimal>,
    pub unit_amount_cents: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegenerateError {
    #[error("fee_not_on_invoice")]
    UnknownFee,
    #[error("fee_adjusted_twice")]
    Duplicate,
    #[error("negative_units_or_amount")]
    Negative,
    #[error("invoice_has_no_fees_to_regenerate")]
    NoFees,
}

fn cents(d: Decimal) -> i64 {
    d.round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero).to_i64().unwrap_or(i64::MAX)
}

/// The re-issued invoice's fees (fresh ids, attached to `new_invoice_id`).
pub fn regenerate_fees(source: &[Fee], adjustments: &[FeeAdjustment], new_invoice_id: Uuid) -> Result<Vec<Fee>, RegenerateError> {
    for (i, a) in adjustments.iter().enumerate() {
        if !source.iter().any(|f| f.id == a.fee_id) {
            return Err(RegenerateError::UnknownFee);
        }
        if adjustments[..i].iter().any(|b| b.fee_id == a.fee_id) {
            return Err(RegenerateError::Duplicate);
        }
        if a.units.is_some_and(|u| u < Decimal::ZERO) || a.unit_amount_cents.is_some_and(|c| c < 0) {
            return Err(RegenerateError::Negative);
        }
    }
    let mut out = Vec::with_capacity(source.len());
    for fee in source {
        let mut copy = fee.clone();
        copy.id = Uuid::new_v4();
        copy.invoice_id = Some(new_invoice_id);
        if let Some(a) = adjustments.iter().find(|a| a.fee_id == fee.id) {
            let units = a.units.unwrap_or(fee.units);
            if units.is_zero() {
                continue;
            }
            let unit_cents = a.unit_amount_cents.unwrap_or(fee.unit_amount_cents);
            let precise = units * Decimal::from(unit_cents);
            copy.units = units;
            copy.unit_amount_cents = unit_cents;
            copy.precise_unit_amount = Decimal::from(unit_cents) / fee.currency().subunit_to_unit_decimal();
            copy.precise_amount_cents = precise;
            copy.amount_cents = cents(precise);
            let rate = Decimal::try_from(fee.taxes_rate).unwrap_or_default();
            let taxes = Decimal::from(copy.amount_cents) * rate / Decimal::ONE_HUNDRED;
            copy.taxes_precise_amount_cents = taxes;
            copy.taxes_amount_cents = cents(taxes);
            copy.precise_coupons_amount_cents = Decimal::ZERO;
            copy.precise_credit_notes_amount_cents = Decimal::ZERO;
            copy.total_aggregated_units = Some(units);
        }
        out.push(copy);
    }
    if out.is_empty() {
        return Err(RegenerateError::NoFees);
    }
    Ok(out)
}

/// Rebuild an invoice's money rollups from its fees. Coupons and credits
/// carried from the voided invoice are kept but never exceed what they apply to.
pub fn retotal(invoice: &mut Invoice, fees: &[Fee]) {
    let fees_amount: i64 = fees.iter().map(|f| f.amount_cents).sum();
    let taxes: i64 = fees.iter().map(|f| f.taxes_amount_cents).sum();
    let coupons = invoice.coupons_amount_cents.clamp(0, fees_amount);
    let sub_excl = fees_amount - coupons;
    let sub_incl = sub_excl + taxes;
    let credits = invoice.credit_notes_amount_cents.max(0);
    let prepaid = invoice.prepaid_credit_amount_cents.max(0);
    let progressive = invoice.progressive_billing_credit_amount_cents.max(0);
    let deductions = (credits + prepaid + progressive).min(sub_incl);
    invoice.fees_amount_cents = fees_amount;
    invoice.coupons_amount_cents = coupons;
    invoice.taxes_amount_cents = taxes;
    invoice.sub_total_excluding_taxes_amount_cents = sub_excl;
    invoice.sub_total_including_taxes_amount_cents = sub_incl;
    invoice.total_amount_cents = sub_incl - deductions;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fee(units: i64, unit: i64, rate: f64) -> Fee {
        let mut f: Fee = serde_json::from_value(serde_json::json!({
            "id": Uuid::new_v4(), "organization_id": Uuid::new_v4(), "billing_entity_id": Uuid::new_v4(),
            "invoice_id": null, "subscription_id": null, "charge_id": null, "charge_filter_id": null,
            "fee_type": null, "invoiceable_type": null, "invoiceable_id": null,
            "amount_cents": units * unit, "amount_currency": "USD", "precise_amount_cents": "0",
            "unit_amount_cents": unit, "precise_unit_amount": "0",
            "taxes_amount_cents": 0, "taxes_precise_amount_cents": "0", "taxes_rate": rate,
            "units": units.to_string(), "total_aggregated_units": null, "events_count": null,
            "precise_coupons_amount_cents": "0", "precise_credit_notes_amount_cents": "0",
            "payment_status": "Pending", "properties": {}, "amount_details": {}, "grouped_by": {},
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-01T00:00:00Z"
        }))
        .expect("fee fixture");
        f.taxes_amount_cents = (f.amount_cents as f64 * rate / 100.0).round() as i64;
        f
    }

    #[test]
    fn without_adjustments_fees_are_copied() {
        let src = vec![fee(3, 500, 0.0)];
        let inv = Uuid::new_v4();
        let out = regenerate_fees(&src, &[], inv).unwrap();
        assert_eq!(out.len(), 1);
        assert_ne!(out[0].id, src[0].id);
        assert_eq!((out[0].invoice_id, out[0].amount_cents), (Some(inv), 1500));
    }

    #[test]
    fn adjustments_recompute_amounts_and_taxes_and_zero_drops() {
        let a = fee(3, 500, 20.0);
        let b = fee(1, 999, 0.0);
        let out = regenerate_fees(
            &[a.clone(), b.clone()],
            &[
                FeeAdjustment { fee_id: a.id, units: Some(Decimal::new(25, 1)), unit_amount_cents: Some(400) },
                FeeAdjustment { fee_id: b.id, units: Some(Decimal::ZERO), unit_amount_cents: None },
            ],
            Uuid::new_v4(),
        )
        .unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!((out[0].amount_cents, out[0].taxes_amount_cents, out[0].unit_amount_cents), (1000, 200, 400));
    }

    #[test]
    fn bad_adjustments_are_refused() {
        let a = fee(1, 100, 0.0);
        let unknown = FeeAdjustment { fee_id: Uuid::new_v4(), units: None, unit_amount_cents: None };
        assert_eq!(regenerate_fees(&[a.clone()], &[unknown], Uuid::new_v4()).err(), Some(RegenerateError::UnknownFee));
        let neg = FeeAdjustment { fee_id: a.id, units: Some(Decimal::NEGATIVE_ONE), unit_amount_cents: None };
        assert_eq!(regenerate_fees(&[a.clone()], &[neg], Uuid::new_v4()).err(), Some(RegenerateError::Negative));
        let zero = FeeAdjustment { fee_id: a.id, units: Some(Decimal::ZERO), unit_amount_cents: None };
        assert_eq!(regenerate_fees(&[a.clone()], &[zero.clone()], Uuid::new_v4()).err(), Some(RegenerateError::NoFees));
        assert_eq!(regenerate_fees(&[a.clone()], &[zero.clone(), zero], Uuid::new_v4()).err(), Some(RegenerateError::Duplicate));
    }
}
