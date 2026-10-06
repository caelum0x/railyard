//! `Fees::FixedChargeService` — a plan's fixed charges (a seat count, a
//! platform fee in units) billed on the periodic invoice.
//!
//! Rago keeps a fixed charge's `units` on the charge itself (Lago's
//! `fixed_charge_events` history of unit changes is not ported), so the
//! aggregation is:
//! * simple: `units` for the period;
//! * prorated: `units * billed days / period days`, with the full `units` as
//!   the fee's units (Lago `ProratedAggregationService`, one effective event).
//!
//! Timing: on a pay-in-advance plan a pay-in-advance fixed charge is billed
//! upfront with the subscription fee for the period ahead; every other fixed
//! charge is billed in arrears with the closed period's usage. (Lago also
//! bills a pay-in-advance fixed charge of a pay-in-arrears plan on the
//! subscription's first invoice; Rago has no start invoice for those plans,
//! so it is billed in arrears: never skipped, never billed twice.)

use rago_core::enums::{ChargeModel, FeeType};
use rago_core::models::{Charge, Fee, Subscription};
use rago_core::money::Currency;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::aggregation::AggregationResult;
use crate::charge_models::{self, PricingStructure};
use crate::error::Result;
use crate::fees::build_charge_fee;
use crate::proration::{DayRange, InvoicePeriod};

/// A stored fixed charge.
#[derive(Debug, Clone)]
pub struct FixedChargeSpec {
    pub id: Uuid,
    pub add_on_id: Option<Uuid>,
    pub charge_model: ChargeModel,
    pub properties: Value,
    pub units: Decimal,
    pub pay_in_advance: bool,
    pub prorated: bool,
    pub invoice_display_name: Option<String>,
}

/// Which part of an invoice a fixed charge is billed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timing {
    /// With the subscription fee, for the period ahead.
    Upfront,
    /// With the closed period's usage.
    InArrears,
}

/// The timing of `fc` on a plan.
pub fn timing(fc: &FixedChargeSpec, plan_pay_in_advance: bool) -> Timing {
    if fc.pay_in_advance && plan_pay_in_advance {
        Timing::Upfront
    } else {
        Timing::InArrears
    }
}

/// The `(billed days, whole period)` a fixed charge covers on an invoice, or
/// `None` when this invoice bills none of it. `upfront_billed` is whether the
/// invoice bills the subscription fee ahead (a closing or final invoice of a
/// pay-in-advance plan does not: its upfront fixed charges were paid).
pub fn coverage(timing: Timing, period: &InvoicePeriod, upfront_billed: bool) -> Option<(DayRange, DayRange)> {
    let (window, whole) = match timing {
        Timing::Upfront if upfront_billed => (period.fee_window, period.fee_period),
        Timing::Upfront => return None,
        Timing::InArrears => (period.charges_window, period.charges_period),
    };
    (!window.is_empty()).then_some((window, whole))
}

/// The fee of one fixed charge for `window` of `whole`.
pub fn fixed_charge_fee(
    fc: &FixedChargeSpec,
    subscription: &Subscription,
    currency: Currency,
    invoice_id: Option<Uuid>,
    window: DayRange,
    whole: DayRange,
) -> Result<Fee> {
    let units = fc.units.max(Decimal::ZERO);
    let ratio = if whole.days() == 0 {
        Decimal::ZERO
    } else {
        Decimal::from(window.days()) / Decimal::from(whole.days())
    };
    let agg = if fc.prorated && ratio < Decimal::ONE {
        let prorated = (units * ratio).round_dp(6);
        AggregationResult {
            aggregation: prorated,
            current_usage_units: Some(units),
            full_units_number: Some(units),
            total_aggregated_units: Some(units),
            event_full_values: vec![units],
            event_prorated_values: vec![prorated],
            ..Default::default()
        }
    } else {
        AggregationResult {
            aggregation: units,
            current_usage_units: Some(units),
            full_units_number: Some(units),
            total_aggregated_units: Some(units),
            ..Default::default()
        }
    };
    let ps = PricingStructure {
        charge_model: fc.charge_model,
        properties: fc.properties.clone(),
        prorated: fc.prorated,
        accepts_target_wallet: false,
        currency,
    };
    let cmr = charge_models::compute(&ps, &agg)?;
    let mut fee = build_charge_fee(cmr, &carrier(fc, subscription), subscription, invoice_id, currency);
    fee.charge_id = None;
    fee.fee_type = Some(FeeType::FixedCharge);
    fee.invoiceable_type = Some("FixedCharge".to_string());
    fee.invoiceable_id = Some(fc.id);
    fee.units = units;
    fee.total_aggregated_units = Some(units);
    fee.events_count = Some(0);
    fee.properties = json!({
        "fixed_charges_from_datetime": window.from.and_hms_opt(0, 0, 0).map(|t| t.and_utc().to_rfc3339()),
        "fixed_charges_to_datetime": window.to.and_hms_opt(23, 59, 59).map(|t| t.and_utc().to_rfc3339()),
        "fixed_charges_duration": whole.days(),
        "fixed_charge_id": fc.id,
        "add_on_id": fc.add_on_id,
        "invoice_display_name": fc.invoice_display_name,
    });
    Ok(fee)
}

/// Every fixed-charge fee an invoice for `period` bills.
pub fn fixed_charge_fees(
    fixed_charges: &[FixedChargeSpec],
    plan_pay_in_advance: bool,
    period: &InvoicePeriod,
    upfront_billed: bool,
    subscription: &Subscription,
    currency: Currency,
    invoice_id: Option<Uuid>,
) -> Result<Vec<Fee>> {
    let mut out = Vec::new();
    for fc in fixed_charges {
        let Some((window, whole)) = coverage(timing(fc, plan_pay_in_advance), period, upfront_billed) else {
            continue;
        };
        out.push(fixed_charge_fee(fc, subscription, currency, invoice_id, window, whole)?);
    }
    Ok(out)
}

/// The fee builder takes a [`Charge`]; a fixed charge borrows its shape.
fn carrier(fc: &FixedChargeSpec, subscription: &Subscription) -> Charge {
    let now = chrono::Utc::now();
    Charge {
        id: fc.id,
        organization_id: subscription.organization_id,
        billable_metric_id: None,
        plan_id: Some(subscription.plan_id),
        code: String::new(),
        charge_model: fc.charge_model,
        properties: fc.properties.clone(),
        pay_in_advance: fc.pay_in_advance,
        prorated: fc.prorated,
        invoiceable: true,
        min_amount_cents: 0,
        amount_currency: None,
        accepts_target_wallet: false,
        invoice_display_name: fc.invoice_display_name.clone(),
        parent_id: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, Utc};
    use rago_core::enums::{BillingTime, SubscriptionStatus};
    use rust_decimal_macros::dec;

    fn d(m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, m, day).unwrap()
    }

    fn sub() -> Subscription {
        let now = Utc::now();
        Subscription {
            id: Uuid::new_v4(),
            organization_id: Uuid::nil(),
            customer_id: Uuid::new_v4(),
            plan_id: Uuid::new_v4(),
            billing_entity_id: None,
            external_id: "s".into(),
            name: None,
            status: SubscriptionStatus::Active,
            billing_time: BillingTime::Calendar,
            subscription_at: Some(now),
            started_at: Some(now),
            ending_at: None,
            trial_ended_at: None,
            canceled_at: None,
            terminated_at: None,
            previous_subscription_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn fc(model: ChargeModel, properties: Value, units: Decimal, prorated: bool, in_advance: bool) -> FixedChargeSpec {
        FixedChargeSpec {
            id: Uuid::new_v4(),
            add_on_id: None,
            charge_model: model,
            properties,
            units,
            pay_in_advance: in_advance,
            prorated,
            invoice_display_name: Some("Seats".into()),
        }
    }

    fn june() -> DayRange {
        DayRange::new(d(6, 1), d(6, 30))
    }

    #[test]
    fn a_full_period_bills_every_unit() {
        let f = fc(ChargeModel::Standard, json!({ "amount": "12" }), dec!(5), false, false);
        let fee = fixed_charge_fee(&f, &sub(), Currency::new("USD"), None, june(), june()).unwrap();
        assert_eq!(fee.amount_cents, 6_000);
        assert_eq!(fee.units, dec!(5));
        assert_eq!(fee.fee_type, Some(FeeType::FixedCharge));
        assert_eq!(fee.invoiceable_id, Some(f.id));
        assert_eq!(fee.charge_id, None);
    }

    #[test]
    fn a_prorated_fixed_charge_bills_the_days_lived_and_a_plain_one_bills_in_full() {
        let window = DayRange::new(d(6, 16), d(6, 30)); // 15 of 30 days
        let p = fc(ChargeModel::Standard, json!({ "amount": "12" }), dec!(5), true, false);
        let fee = fixed_charge_fee(&p, &sub(), Currency::new("USD"), None, window, june()).unwrap();
        assert_eq!(fee.amount_cents, 3_000);
        assert_eq!(fee.units, dec!(5), "the fee shows the full units");
        let full = fc(ChargeModel::Standard, json!({ "amount": "12" }), dec!(5), false, false);
        assert_eq!(
            fixed_charge_fee(&full, &sub(), Currency::new("USD"), None, window, june())
                .unwrap()
                .amount_cents,
            6_000
        );
    }

    #[test]
    fn graduated_fixed_charges_price_through_the_ranges() {
        let g = fc(
            ChargeModel::Graduated,
            json!({ "graduated_ranges": [
                { "from_value": 0, "to_value": 3, "per_unit_amount": "10", "flat_amount": "0" },
                { "from_value": 4, "to_value": null, "per_unit_amount": "5", "flat_amount": "0" }
            ]}),
            dec!(5),
            false,
            false,
        );
        // 3 * 10 + 2 * 5 = 40.
        assert_eq!(
            fixed_charge_fee(&g, &sub(), Currency::new("USD"), None, june(), june())
                .unwrap()
                .amount_cents,
            4_000
        );
    }

    #[test]
    fn upfront_charges_follow_the_fee_and_the_rest_follow_usage() {
        let period = InvoicePeriod {
            fee_period: DayRange::new(d(7, 1), d(7, 31)),
            fee_window: DayRange::new(d(7, 1), d(7, 31)),
            charges_period: june(),
            charges_window: june(),
        };
        let upfront = fc(ChargeModel::Standard, json!({ "amount": "1" }), dec!(1), false, true);
        let arrears = fc(ChargeModel::Standard, json!({ "amount": "1" }), dec!(1), false, false);
        assert_eq!(
            coverage(timing(&upfront, true), &period, true).unwrap().0,
            period.fee_window
        );
        assert_eq!(coverage(timing(&arrears, true), &period, true).unwrap().0, june());
        // A closing invoice of a pay-in-advance plan: the upfront charge was paid.
        assert!(coverage(timing(&upfront, true), &period, false).is_none());
        // On a pay-in-arrears plan everything is billed in arrears.
        assert_eq!(timing(&upfront, false), Timing::InArrears);
        // No usage window (a pay-in-advance plan's first invoice): nothing in arrears.
        let first = InvoicePeriod {
            charges_window: DayRange::new(d(6, 2), d(6, 1)),
            ..period
        };
        assert!(coverage(Timing::InArrears, &first, true).is_none());
        let fees = fixed_charge_fees(
            &[upfront, arrears],
            true,
            &period,
            true,
            &sub(),
            Currency::new("USD"),
            None,
        )
        .unwrap();
        assert_eq!(fees.len(), 2);
    }
}
