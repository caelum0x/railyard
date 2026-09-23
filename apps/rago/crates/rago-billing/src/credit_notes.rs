//! Credit notes.
//!
//! Faithful port of `CreditNotes::CreateService` / `EstimateService` +
//! `CreditNotes::ApplyTaxesService`: build a credit note from an invoice by
//! crediting fee amounts (items), computing the coupon adjustment and taxes at
//! item level, and splitting into credit vs refund amounts.
//!
//! Item taxes reuse the invoice's applied-tax rates. Each item's taxable base
//! is `item_amount − prorated_coupon`, where the prorated coupon is
//! `fee.precise_coupons_amount_cents × (item_amount / fee.amount_cents)`.

use std::collections::BTreeMap;

use chrono::Utc;
use rago_core::models::{
    CreditNote, CreditNoteAppliedTax, CreditNoteItem, Fee, Invoice, InvoiceAppliedTax,
};
use rago_core::money::round_to_integer;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use uuid::Uuid;

/// How much of the credit note is credited (to wallet/balance) vs refunded
/// (cash). `Full` credits everything; `Refund` refunds everything; `Split`
/// takes an explicit refund portion.
#[derive(Debug, Clone, Copy)]
pub enum CreditRefund {
    /// Credit the whole amount (no cash refund).
    CreditOnly,
    /// Refund the whole amount (no balance credit).
    RefundOnly,
    /// Explicit refund cents; the remainder is credited.
    Split { refund_amount_cents: i64 },
}

/// Result of `CreditNotes::ApplyTaxesService`.
struct CreditNoteTaxes {
    coupons_adjustment_amount_cents: Decimal,
    precise_taxes_amount_cents: Decimal,
    taxes_amount_cents: i64,
    taxes_rate: f64,
    applied_taxes: Vec<CreditNoteAppliedTax>,
}

/// `CreditNotes::ApplyTaxesService` — compute the item-level coupon adjustment
/// and taxes from the invoice's applied taxes.
fn apply_taxes(
    invoice: &Invoice,
    fees_by_id: &BTreeMap<Uuid, &Fee>,
    items: &[CreditNoteItem],
) -> CreditNoteTaxes {
    let hundred = dec!(100);

    // item_fee_rate = item.precise / fee.amount_cents ; prorated coupon per item.
    let item_fee_rate = |item: &CreditNoteItem| -> Decimal {
        match fees_by_id.get(&item.fee_id) {
            Some(fee) if fee.amount_cents != 0 => {
                item.precise_amount_cents / Decimal::from(fee.amount_cents)
            }
            _ => Decimal::ZERO,
        }
    };
    let prorated_coupon = |item: &CreditNoteItem| -> Decimal {
        match fees_by_id.get(&item.fee_id) {
            Some(fee) => fee.precise_coupons_amount_cents * item_fee_rate(item),
            None => Decimal::ZERO,
        }
    };

    // coupons_adjustment = Σ fee.precise_coupons * item_fee_rate.
    let coupons_adjustment: Decimal = items.iter().map(prorated_coupon).sum();
    let items_amount: Decimal = items.iter().map(|i| i.precise_amount_cents).sum();

    // Index items by tax_code (from each item's fee's applied taxes).
    let mut order: Vec<String> = Vec::new();
    let mut by_tax: BTreeMap<String, Vec<&CreditNoteItem>> = BTreeMap::new();
    for item in items {
        if let Some(fee) = fees_by_id.get(&item.fee_id) {
            for at in &fee.applied_taxes {
                if !by_tax.contains_key(&at.tax_code) {
                    order.push(at.tax_code.clone());
                }
                by_tax.entry(at.tax_code.clone()).or_default().push(item);
            }
        }
    }

    let base_for_tax = |code: &str| -> Decimal {
        by_tax
            .get(code)
            .map(|its| {
                its.iter()
                    .map(|it| it.precise_amount_cents - prorated_coupon(it))
                    .sum()
            })
            .unwrap_or(Decimal::ZERO)
    };

    let mut precise_taxes = Decimal::ZERO;
    let mut taxes_amount: i64 = 0;
    let mut taxes_rate = 0.0f64;
    let mut applied: Vec<CreditNoteAppliedTax> = Vec::new();
    let total_items_amount = items_amount - coupons_adjustment;

    for code in &order {
        let iat = find_invoice_applied_tax(invoice, code);
        let tax_rate = iat.map(|t| t.tax_rate).unwrap_or(0.0);
        let rate_dec = Decimal::from_f64_retain(tax_rate).unwrap_or(Decimal::ZERO);

        let base = base_for_tax(code);
        // taxes_base_rate defaults to 1 (no partial invoice-level adjustment).
        let precise_base = base;
        let precise_tax = precise_base * rate_dec / hundred;

        applied.push(CreditNoteAppliedTax {
            id: Uuid::new_v4(),
            organization_id: invoice.organization_id,
            credit_note_id: None,
            tax_id: iat.map(|t| t.tax_id).unwrap_or_default(),
            tax_code: code.clone(),
            tax_name: iat.map(|t| t.tax_name.clone()).unwrap_or_default(),
            tax_rate,
            tax_description: iat.and_then(|t| t.tax_description.clone()),
            amount_cents: round_to_integer(precise_tax).to_i64().unwrap_or(0),
            base_amount_cents: round_to_integer(precise_base).to_i64().unwrap_or(0),
            amount_currency: invoice.currency().code(),
        });

        precise_taxes += precise_tax;
        taxes_amount += round_to_integer(precise_tax).to_i64().unwrap_or(0);

        // pro_rated_taxes_rate.
        let items_rate = if total_items_amount.is_zero() {
            Decimal::ZERO
        } else {
            base / total_items_amount
        };
        taxes_rate += (items_rate * rate_dec).to_f64().unwrap_or(0.0);
    }

    CreditNoteTaxes {
        coupons_adjustment_amount_cents: coupons_adjustment,
        precise_taxes_amount_cents: precise_taxes,
        taxes_amount_cents: taxes_amount,
        taxes_rate: (taxes_rate * 100000.0).round() / 100000.0,
        applied_taxes: applied,
    }
}

fn find_invoice_applied_tax<'a>(invoice: &'a Invoice, code: &str) -> Option<&'a InvoiceAppliedTax> {
    invoice.applied_taxes.iter().find(|t| t.tax_code == code)
}

/// `CreditNotes::CreateService` — build a credit note from `items` against an
/// invoice's `fees`, splitting the creditable amount per `refund`.
///
/// `credit_amount = Σ item_amount − coupons_adjustment + precise_taxes`
/// (`compute_creditable_amount`); `total = credit + refund`;
/// `balance = credit`.
///
/// `sequential_id` is the credit note's per-invoice ordinal — the caller passes
/// `MAX(sequential_id) + 1` for the invoice (see
/// [`rago_db::Db::next_credit_note_sequential_id`]). Both `sequential_id` and
/// the derived `number` (`<invoice>-CN<n>`) come from it, so sibling credit
/// notes never collide on `-CN1`.
pub fn create_credit_note(
    invoice: &Invoice,
    fees: &[Fee],
    items: Vec<CreditNoteItem>,
    refund: CreditRefund,
    sequential_id: i32,
) -> CreditNote {
    let fees_by_id: BTreeMap<Uuid, &Fee> = fees.iter().map(|f| (f.id, f)).collect();
    let taxes = apply_taxes(invoice, &fees_by_id, &items);

    let items_total: Decimal = items.iter().map(|i| Decimal::from(i.amount_cents)).sum();
    let creditable = round_to_integer(
        items_total - taxes.coupons_adjustment_amount_cents + taxes.precise_taxes_amount_cents,
    )
    .to_i64()
    .unwrap_or(0);

    let (credit_amount_cents, refund_amount_cents) = match refund {
        CreditRefund::CreditOnly => (creditable, 0),
        CreditRefund::RefundOnly => (0, creditable),
        CreditRefund::Split {
            refund_amount_cents,
        } => {
            let refund = refund_amount_cents.clamp(0, creditable);
            (creditable - refund, refund)
        }
    };

    let total_amount_cents = credit_amount_cents + refund_amount_cents;
    let now = Utc::now();

    let mut applied_taxes = taxes.applied_taxes;
    let cn_id = Uuid::new_v4();
    for at in applied_taxes.iter_mut() {
        at.credit_note_id = Some(cn_id);
    }

    CreditNote {
        id: cn_id,
        organization_id: invoice.organization_id,
        customer_id: invoice.customer_id.unwrap_or_default(),
        invoice_id: invoice.id,
        sequential_id,
        number: format!("{}-CN{sequential_id}", invoice.number),
        issuing_date: now.date_naive(),
        reason: 0,
        credit_status: Some(0),
        refund_status: if refund_amount_cents > 0 {
            Some(0)
        } else {
            None
        },
        status: invoice.status.as_i32(),
        credit_amount_cents,
        credit_amount_currency: invoice.currency().code(),
        balance_amount_cents: credit_amount_cents,
        refund_amount_cents,
        total_amount_cents,
        total_amount_currency: invoice.currency().code(),
        taxes_amount_cents: taxes.taxes_amount_cents,
        taxes_rate: taxes.taxes_rate,
        coupons_adjustment_amount_cents: round_to_integer(taxes.coupons_adjustment_amount_cents)
            .to_i64()
            .unwrap_or(0),
        precise_coupons_adjustment_amount_cents: taxes.coupons_adjustment_amount_cents,
        precise_taxes_amount_cents: taxes.precise_taxes_amount_cents,
        created_at: now,
        updated_at: now,
        voided_at: None,
        items: {
            let mut its = items;
            for it in its.iter_mut() {
                it.credit_note_id = Some(cn_id);
            }
            its
        },
        applied_taxes,
    }
}

/// Apply a credit note's credit amount back to its invoice
/// (`invoice.credit_notes_amount_cents += credit_amount_cents`), reducing the
/// amount still owed. Returns the updated invoice total.
pub fn apply_credit_note_to_invoice(invoice: &mut Invoice, credit_note: &CreditNote) {
    invoice.credit_notes_amount_cents += credit_note.credit_amount_cents;
    invoice.total_amount_cents = invoice.sub_total_including_taxes_amount_cents
        - invoice.credit_notes_amount_cents
        - invoice.prepaid_credit_amount_cents;
}
