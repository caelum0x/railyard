//! Invoice assembly.
//!
//! Ports the invoice-level money pipeline:
//! * `Credits::AppliedCouponService` / `AppliedCoupons::AmountService` — apply
//!   coupons to the fees and the invoice.
//! * `Invoices::ComputeAmountsFromFees` — roll fees up into invoice totals.
//! * `Invoices::ApplyTaxesService` — invoice-level tax rollup from fee taxes.

use rago_core::models::{AppliedCoupon, Coupon, Credit, Fee, Invoice, InvoiceAppliedTax, Tax};
use rago_core::money::round_to_integer;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use uuid::Uuid;

use crate::fees;

/// A coupon plus its customer application, as consumed by [`assemble_invoice`].
pub struct CouponApplication<'a> {
    pub applied_coupon: &'a AppliedCoupon,
    pub coupon: &'a Coupon,
    /// When the coupon is limited to specific plans or billable metrics
    /// (`limited_plans` / `limited_billable_metrics`), the ids of the fees it
    /// may be applied to. `None` means the coupon applies to the whole invoice.
    pub limited_to_fee_ids: Option<Vec<Uuid>>,
}

impl<'a> CouponApplication<'a> {
    /// Unlimited coupon over the whole invoice.
    pub fn new(applied_coupon: &'a AppliedCoupon, coupon: &'a Coupon) -> Self {
        Self {
            applied_coupon,
            coupon,
            limited_to_fee_ids: None,
        }
    }
}

/// `AppliedCoupon` state after applying `credit_amount` cents on an invoice —
/// mirrors `Credits::AppliedCouponService`'s `should_terminate_applied_coupon?`
/// and `decrement_frequency_duration_remaining`.
///
/// * once: terminated once applied (percentage, or the remaining amount consumed).
/// * recurring: `frequency_duration_remaining` is decremented; terminated at 0.
/// * forever: never terminated.
pub fn applied_coupon_after(
    app: &CouponApplication,
    credit_amount_cents: Decimal,
) -> AppliedCoupon {
    let mut ac = app.applied_coupon.clone();
    if ac.is_forever() {
        return ac;
    }

    if ac.is_once() {
        let terminate =
            app.coupon.is_percentage() || credit_amount_cents >= ac.remaining_amount(Decimal::ZERO);
        if terminate {
            ac.status = rago_core::enums::CouponStatus::Terminated;
            ac.terminated_at = Some(chrono::Utc::now());
        }
    } else {
        // recurring
        let remaining = (ac.frequency_duration_remaining.unwrap_or(0) - 1).max(0);
        ac.frequency_duration_remaining = Some(remaining);
        if remaining <= 0 {
            ac.status = rago_core::enums::CouponStatus::Terminated;
            ac.terminated_at = Some(chrono::Utc::now());
        }
    }
    ac
}

/// `AppliedCoupons::AmountService#compute_amount` — the credit a single coupon
/// yields against `base_amount_cents`.
fn coupon_amount(app: &CouponApplication, base_amount_cents: Decimal) -> Decimal {
    if app.coupon.is_percentage() {
        let rate = app.applied_coupon.percentage_rate.unwrap_or(Decimal::ZERO);
        let discounted = base_amount_cents * (rate / dec!(100));
        return if discounted >= base_amount_cents {
            base_amount_cents
        } else {
            round_to_integer(discounted)
        };
    }

    if app.applied_coupon.is_recurring() || app.applied_coupon.is_forever() {
        let amount = Decimal::from(app.applied_coupon.amount_cents.unwrap_or(0));
        if amount > base_amount_cents {
            base_amount_cents
        } else {
            amount
        }
    } else {
        // once
        let remaining = app.applied_coupon.remaining_amount(Decimal::ZERO);
        if remaining > base_amount_cents {
            base_amount_cents
        } else {
            remaining
        }
    }
}

/// `Credits::AppliedCouponService#call` for one coupon: create the credit and
/// distribute its amount across the fees (weighted by remaining fee amount).
fn apply_one_coupon(
    invoice: &mut Invoice,
    fees: &mut [Fee],
    app: &CouponApplication,
) -> Option<Credit> {
    // Which fee indices this coupon applies to (all, or the limited subset).
    let applies_to = |fee: &Fee| -> bool {
        match &app.limited_to_fee_ids {
            None => true,
            Some(ids) => ids.contains(&fee.id),
        }
    };

    // base_amount_cents: for limited coupons, the sum of the remaining amount of
    // the matching fees; otherwise the invoice sub-total.
    let base_amount_cents = if app.limited_to_fee_ids.is_some() {
        fees.iter()
            .filter(|f| applies_to(f))
            .map(|f| Decimal::from(f.amount_cents) - f.precise_coupons_amount_cents)
            .sum::<Decimal>()
    } else {
        Decimal::from(invoice.sub_total_excluding_taxes_amount_cents)
    };
    if !base_amount_cents.is_sign_positive() || base_amount_cents.is_zero() {
        return None;
    }

    let credit_amount = coupon_amount(app, base_amount_cents);
    if credit_amount.is_zero() {
        return None;
    }

    // Distribute across the applicable fees (weighting base stays constant).
    let weighting_base = base_amount_cents;
    for fee in fees.iter_mut().filter(|f| applies_to(f)) {
        if !weighting_base.is_zero() {
            fee.precise_coupons_amount_cents +=
                fee.compute_precise_credit_amount_cents(credit_amount, weighting_base);
        }
        if Decimal::from(fee.amount_cents) < fee.precise_coupons_amount_cents {
            fee.precise_coupons_amount_cents = Decimal::from(fee.amount_cents);
        }
    }

    let credit_amount_cents = round_to_integer(credit_amount).to_i64().unwrap_or(0);
    invoice.coupons_amount_cents += credit_amount_cents;
    invoice.sub_total_excluding_taxes_amount_cents -= credit_amount_cents;

    Some(Credit {
        id: Uuid::new_v4(),
        organization_id: invoice.organization_id,
        invoice_id: Some(invoice.id),
        applied_coupon_id: Some(app.applied_coupon.id),
        credit_note_id: None,
        progressive_billing_invoice_id: None,
        amount_cents: credit_amount_cents,
        amount_currency: invoice.currency().code(),
        before_taxes: true,
        created_at: invoice.created_at,
        updated_at: invoice.updated_at,
    })
}

/// `Invoices::ApplyTaxesService` — build invoice-level applied taxes from the
/// taxes already attached to the fees, and roll up `taxes_amount_cents` /
/// `taxes_rate`.
fn apply_invoice_taxes(invoice: &mut Invoice, fees: &[Fee]) {
    // Index fees by tax id (tax_id -> fee sub-totals + rate/meta).
    struct TaxAgg {
        rate: f64,
        code: String,
        name: String,
        description: Option<String>,
        fees_sub_total: Decimal,
    }
    // Preserve first-seen order of taxes.
    let mut order: Vec<Uuid> = Vec::new();
    let mut map: std::collections::HashMap<Uuid, TaxAgg> = std::collections::HashMap::new();

    for fee in fees {
        let fee_sub_total = fee.sub_total_excluding_taxes_amount_cents();
        for at in &fee.applied_taxes {
            let entry = map.entry(at.tax_id).or_insert_with(|| {
                order.push(at.tax_id);
                TaxAgg {
                    rate: at.tax_rate,
                    code: at.tax_code.clone(),
                    name: at.tax_name.clone(),
                    description: at.tax_description.clone(),
                    fees_sub_total: Decimal::ZERO,
                }
            });
            entry.fees_sub_total += fee_sub_total;
        }
    }

    let hundred = dec!(100);
    let mut applied_taxes_amount_cents = Decimal::ZERO;
    let mut taxes_rate = 0.0f64;
    let sub_total = Decimal::from(invoice.sub_total_excluding_taxes_amount_cents);
    let fees_len = fees.len() as i64;

    let mut applied: Vec<InvoiceAppliedTax> = Vec::new();
    for tax_id in &order {
        let agg = &map[tax_id];
        let rate_dec = Decimal::from_f64_retain(agg.rate).unwrap_or(Decimal::ZERO);
        let tax_amount_cents = (agg.fees_sub_total * rate_dec) / hundred;

        applied.push(InvoiceAppliedTax {
            id: Uuid::new_v4(),
            organization_id: invoice.organization_id,
            invoice_id: invoice.id,
            tax_id: *tax_id,
            tax_code: agg.code.clone(),
            tax_name: agg.name.clone(),
            tax_rate: agg.rate,
            tax_description: agg.description.clone(),
            amount_cents: round_to_integer(tax_amount_cents).to_i64().unwrap_or(0),
            fees_amount_cents: agg.fees_sub_total.to_i64().unwrap_or(0),
            amount_currency: invoice.currency().code(),
        });

        applied_taxes_amount_cents += tax_amount_cents;

        // pro_rated_taxes_rate(tax)
        let fees_rate = if sub_total.is_sign_positive() && !sub_total.is_zero() {
            agg.fees_sub_total / sub_total
        } else {
            // number-of-fees prorata when the invoice amount is zero
            let count = fees
                .iter()
                .filter(|f| f.applied_taxes.iter().any(|t| t.tax_id == *tax_id))
                .count() as i64;
            if fees_len == 0 {
                Decimal::ZERO
            } else {
                Decimal::from(count) / Decimal::from(fees_len)
            }
        };
        taxes_rate += (fees_rate * rate_dec).to_f64().unwrap_or(0.0);
    }

    invoice.taxes_amount_cents = round_to_integer(applied_taxes_amount_cents)
        .to_i64()
        .unwrap_or(0);
    invoice.taxes_rate = (taxes_rate * 100000.0).round() / 100000.0; // round(5)
    invoice.applied_taxes = applied;
}

/// The end-to-end invoice money pipeline: apply coupons, then
/// `Invoices::ComputeAmountsFromFees`.
///
/// `taxes` are the taxes applicable to each charge fee (from the charge/plan).
/// Returns the coupon credits created.
pub fn assemble_invoice(
    invoice: &mut Invoice,
    fees: &mut [Fee],
    taxes: &[Tax],
    coupons: &[CouponApplication],
) -> Vec<Credit> {
    // Base for coupons: fees amount before any discount.
    let fees_amount: i64 = fees.iter().map(|f| f.amount_cents).sum();
    invoice.fees_amount_cents = fees_amount;
    invoice.sub_total_excluding_taxes_amount_cents = fees_amount;

    // Apply coupons (before taxes) — mutates fee coupon shares.
    let mut credits = Vec::new();
    for app in coupons {
        if invoice.sub_total_excluding_taxes_amount_cents <= 0 {
            break;
        }
        if let Some(credit) = apply_one_coupon(invoice, fees, app) {
            credits.push(credit);
        }
    }

    // Invoices::ComputeAmountsFromFees
    for fee in fees.iter_mut() {
        fees::apply_taxes(fee, taxes);
    }

    invoice.fees_amount_cents = fees.iter().map(|f| f.amount_cents).sum();
    invoice.coupons_amount_cents = credits
        .iter()
        .filter(|c| c.applied_coupon_id.is_some())
        .map(|c| c.amount_cents)
        .sum();

    invoice.sub_total_excluding_taxes_amount_cents = invoice.fees_amount_cents
        - invoice.progressive_billing_credit_amount_cents
        - invoice.coupons_amount_cents;

    apply_invoice_taxes(invoice, fees);

    invoice.sub_total_including_taxes_amount_cents =
        invoice.sub_total_excluding_taxes_amount_cents + invoice.taxes_amount_cents;
    invoice.total_amount_cents =
        invoice.sub_total_including_taxes_amount_cents - invoice.credit_notes_amount_cents;

    credits
}
