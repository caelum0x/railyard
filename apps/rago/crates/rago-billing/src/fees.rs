//! Fee building — port of `Fees::ChargeService#init_fee` (the decimal→cents
//! conversion) and `Fees::ApplyTaxesService`.

use chrono::Utc;
use rago_core::enums::{FeeType, PaymentStatus};
use rago_core::models::{Charge, Fee, FeeAppliedTax, Subscription, Tax};
use rago_core::money::{round_half_up, round_to_integer, Currency};
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::json;
use uuid::Uuid;

use crate::charge_models::ChargeModelResult;

/// `Fees::ChargeService#init_fee` — turn a [`ChargeModelResult`] (amounts in
/// the currency's main unit) into a persisted-shape [`Fee`] with cents.
///
/// This is the in-arrears charge-fee path: `units` come straight from the
/// aggregation (no proration / pay-in-advance current-usage branch).
pub fn build_charge_fee(
    mut cmr: ChargeModelResult,
    charge: &Charge,
    subscription: &Subscription,
    invoice_id: Option<Uuid>,
    currency: Currency,
) -> Fee {
    // Prevent negative units or amount (Lago clamps both to zero).
    if cmr.units.is_sign_negative() || cmr.amount.is_sign_negative() {
        cmr.amount = Decimal::ZERO;
        cmr.unit_amount = Decimal::ZERO;
        cmr.full_units_number = Some(Decimal::ZERO);
        cmr.units = Decimal::ZERO;
    }

    let subunit = currency.subunit_to_unit_decimal();

    // rounded_amount = amount.round(currency.exponent); amount_cents = rounded * subunit
    let rounded_amount = round_half_up(cmr.amount, currency.exponent());
    let amount_cents = (rounded_amount * subunit).to_i64().unwrap_or(0);
    let precise_amount_cents = cmr.amount * subunit;
    // unit_amount_cents is a bigint column: BigDecimal assigned to it truncates.
    let unit_amount_cents = (cmr.unit_amount * subunit).trunc().to_i64().unwrap_or(0);
    let precise_unit_amount = cmr.unit_amount;

    let units = cmr.units;
    let total_aggregated_units = cmr.total_aggregated_units.unwrap_or(units);

    Fee {
        id: Uuid::new_v4(),
        organization_id: subscription.organization_id,
        billing_entity_id: subscription
            .billing_entity_id
            .unwrap_or(subscription.organization_id),
        invoice_id,
        subscription_id: Some(subscription.id),
        charge_id: Some(charge.id),
        charge_filter_id: None,
        fee_type: Some(FeeType::Charge),
        invoiceable_type: Some("Charge".to_string()),
        invoiceable_id: Some(charge.id),
        amount_cents,
        amount_currency: currency.code(),
        precise_amount_cents,
        unit_amount_cents,
        precise_unit_amount,
        taxes_amount_cents: 0,
        taxes_precise_amount_cents: Decimal::ZERO,
        taxes_rate: 0.0,
        units,
        total_aggregated_units: Some(total_aggregated_units),
        events_count: Some(cmr.count as i32),
        precise_coupons_amount_cents: Decimal::ZERO,
        precise_credit_notes_amount_cents: Decimal::ZERO,
        payment_status: PaymentStatus::Pending,
        properties: json!({}),
        amount_details: cmr.amount_details,
        grouped_by: json!({}),
        created_at: Utc::now(),
        updated_at: Utc::now(),
        applied_taxes: Vec::new(),
    }
}

/// `Fees::ApplyTaxesService` — attach the given taxes to a fee and roll up the
/// fee's tax totals. Coupons are applied *before* taxes, so the taxable base is
/// `sub_total_excluding_taxes` (amount minus the fee's coupon share).
pub fn apply_taxes(fee: &mut Fee, taxes: &[Tax]) {
    if !fee.applied_taxes.is_empty() {
        return;
    }

    let hundred = dec!(100);
    let mut applied_taxes_amount_cents = Decimal::ZERO;
    let mut applied_precise_taxes_amount_cents = Decimal::ZERO;
    let mut applied_taxes_rate = 0.0f64;

    let sub_total = fee.sub_total_excluding_taxes_amount_cents();
    let sub_total_precise = fee.sub_total_excluding_taxes_precise_amount_cents();

    let mut applied: Vec<FeeAppliedTax> = Vec::new();
    for tax in taxes {
        let rate = Decimal::from_f64_retain(tax.rate).unwrap_or(Decimal::ZERO);
        let tax_amount_cents = (sub_total * rate) / hundred;
        let tax_precise_amount_cents = (sub_total_precise * rate) / hundred;

        applied.push(FeeAppliedTax {
            id: Uuid::new_v4(),
            organization_id: fee.organization_id,
            fee_id: fee.id,
            tax_id: tax.id,
            tax_code: tax.code.clone(),
            tax_name: tax.name.clone(),
            tax_rate: tax.rate,
            tax_description: tax.description.clone(),
            amount_cents: round_to_integer(tax_amount_cents).to_i64().unwrap_or(0),
            precise_amount_cents: tax_precise_amount_cents,
            amount_currency: fee.amount_currency.clone(),
        });

        applied_taxes_amount_cents += tax_amount_cents;
        applied_precise_taxes_amount_cents += tax_precise_amount_cents;
        applied_taxes_rate += tax.rate;
    }

    fee.taxes_amount_cents = round_to_integer(applied_taxes_amount_cents)
        .to_i64()
        .unwrap_or(0);
    fee.taxes_precise_amount_cents = applied_precise_taxes_amount_cents;
    fee.taxes_rate = applied_taxes_rate;
    fee.applied_taxes = applied;
}

/// One line of an external tax provider's breakdown (name, fractional rate,
/// exact tax amount cents) — the primitive form of
/// `Integrations::Aggregator::Taxes::TaxResult::TaxBreakdownItem`, so this crate
/// stays independent of `rago-integrations`.
#[derive(Debug, Clone)]
pub struct ProviderTaxLine {
    pub name: String,
    /// Fractional rate (e.g. `0.085` for 8.5%).
    pub rate: f64,
    pub tax_amount: i64,
    pub type_: String,
}

/// `Fees::ApplyProviderTaxesService` — apply an external tax provider's
/// breakdown to a fee. The `taxes_base_rate` reconciles the provider's exact
/// tax amount against the naive `sub_total × rate` (proration for partial-tax
/// jurisdictions), exactly as Lago does.
pub fn apply_provider_taxes(fee: &mut Fee, breakdown: &[ProviderTaxLine]) {
    if !fee.applied_taxes.is_empty() {
        return;
    }
    let hundred = dec!(100);
    let sub_total = fee.sub_total_excluding_taxes_amount_cents();
    let sub_total_precise = fee.sub_total_excluding_taxes_precise_amount_cents();

    // taxes_base_rate from the first breakdown line.
    let taxes_base_rate = match breakdown.first() {
        None => Decimal::ONE,
        Some(tax) => {
            let tax_rate = Decimal::from_f64_retain(tax.rate * 100.0).unwrap_or(Decimal::ZERO);
            let naive = (sub_total * tax_rate) / hundred;
            let provider = Decimal::from(tax.tax_amount);
            if !naive.is_zero() && provider < naive {
                provider / naive
            } else {
                Decimal::ONE
            }
        }
    };

    let mut applied_taxes_amount_cents = Decimal::ZERO;
    let mut applied_precise_taxes_amount_cents = Decimal::ZERO;
    let mut applied_taxes_rate = 0.0f64;
    let mut applied: Vec<FeeAppliedTax> = Vec::new();

    for tax in breakdown {
        let tax_rate_pct = tax.rate * 100.0;
        let tax_rate = Decimal::from_f64_retain(tax_rate_pct).unwrap_or(Decimal::ZERO);
        let tax_amount_cents = (sub_total * taxes_base_rate * tax_rate) / hundred;
        let tax_precise = (sub_total_precise * taxes_base_rate * tax_rate) / hundred;

        applied.push(FeeAppliedTax {
            id: Uuid::new_v4(),
            organization_id: fee.organization_id,
            fee_id: fee.id,
            tax_id: Uuid::nil(),
            tax_code: tax.name.to_lowercase().replace(' ', "_"),
            tax_name: tax.name.clone(),
            tax_rate: tax_rate_pct,
            tax_description: Some(tax.type_.clone()),
            amount_cents: round_to_integer(tax_amount_cents).to_i64().unwrap_or(0),
            precise_amount_cents: tax_precise,
            amount_currency: fee.amount_currency.clone(),
        });

        applied_taxes_amount_cents += tax_amount_cents;
        applied_precise_taxes_amount_cents += tax_precise;
        applied_taxes_rate += tax_rate_pct;
    }

    fee.taxes_amount_cents = round_to_integer(applied_taxes_amount_cents)
        .to_i64()
        .unwrap_or(0);
    fee.taxes_precise_amount_cents = applied_precise_taxes_amount_cents;
    fee.taxes_rate = applied_taxes_rate;
    fee.applied_taxes = applied;
}
