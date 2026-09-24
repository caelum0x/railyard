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
    // Accumulated as Decimal to avoid f64 drift; converted to f64 only at the
    // final serialization boundary below.
    let mut taxes_rate_dec = Decimal::ZERO;
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
        taxes_rate_dec += items_rate * rate_dec;
    }

    CreditNoteTaxes {
        coupons_adjustment_amount_cents: coupons_adjustment,
        precise_taxes_amount_cents: precise_taxes,
        taxes_amount_cents: taxes_amount,
        // Convert to f64 only at the serialization boundary; round_dp(5)
        // matches Lago's `round(5)` display precision.
        taxes_rate: taxes_rate_dec.round_dp(5).to_f64().unwrap_or(0.0),
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

/// A credit note's remaining balance, as a candidate for consumption by a
/// later invoice. Callers pass candidates oldest first (issuing date, then
/// creation time), which is the order Lago's `Credits::CreditNoteService`
/// consumes them in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreditNoteBalance {
    pub credit_note_id: Uuid,
    pub balance_amount_cents: i64,
}

/// One credit note's contribution to an invoice: the `credits` row to write
/// and the note's balance after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreditNoteApplication {
    pub credit_note_id: Uuid,
    /// Amount taken from the note and credited on the invoice.
    pub amount_cents: i64,
    /// The note's `balance_amount_cents` after this application.
    pub remaining_balance_cents: i64,
}

impl CreditNoteApplication {
    /// Whether this application used the note up (`credit_status` becomes
    /// `consumed`).
    pub fn consumed(&self) -> bool {
        self.remaining_balance_cents == 0
    }
}

/// `Credits::CreditNoteService`: consume available credit-note balances
/// against `amount_due_cents` (the invoice total after coupons and taxes),
/// oldest note first. Each note gives `min(balance, still_due)`; a note is
/// only partially drawn when the invoice runs out first, and keeps the rest of
/// its balance for later invoices. Notes with no balance are skipped, and
/// nothing is applied to an invoice with nothing due.
pub fn allocate_credit_notes(
    amount_due_cents: i64,
    notes: &[CreditNoteBalance],
) -> Vec<CreditNoteApplication> {
    let mut remaining_due = amount_due_cents.max(0);
    let mut out = Vec::new();
    for note in notes {
        if remaining_due == 0 {
            break;
        }
        if note.balance_amount_cents <= 0 {
            continue;
        }
        let amount = note.balance_amount_cents.min(remaining_due);
        remaining_due -= amount;
        out.push(CreditNoteApplication {
            credit_note_id: note.credit_note_id,
            amount_cents: amount,
            remaining_balance_cents: note.balance_amount_cents - amount,
        });
    }
    out
}

/// Apply allocated credit-note amounts to `invoice`: they add to
/// `credit_notes_amount_cents` and come off `total_amount_cents` (whatever is
/// still due after taxes and any credit already applied). An invoice brought
/// to zero owes nothing and is marked paid when it was still pending.
pub fn apply_credit_note_applications(invoice: &mut Invoice, applications: &[CreditNoteApplication]) {
    let applied: i64 = applications.iter().map(|a| a.amount_cents).sum();
    if applied == 0 {
        return;
    }
    invoice.credit_notes_amount_cents += applied;
    invoice.total_amount_cents = (invoice.total_amount_cents - applied).max(0);
    if invoice.total_amount_cents == 0
        && matches!(invoice.payment_status, rago_core::enums::PaymentStatus::Pending)
    {
        invoice.payment_status = rago_core::enums::PaymentStatus::Succeeded;
    }
}

#[cfg(test)]
mod allocation_tests {
    use super::*;

    fn note(balance: i64) -> CreditNoteBalance {
        CreditNoteBalance {
            credit_note_id: Uuid::new_v4(),
            balance_amount_cents: balance,
        }
    }

    #[test]
    fn oldest_note_is_consumed_first() {
        let notes = [note(300), note(500)];
        let apps = allocate_credit_notes(400, &notes);
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].credit_note_id, notes[0].credit_note_id);
        assert_eq!(apps[0].amount_cents, 300);
        assert!(apps[0].consumed());
        assert_eq!(apps[1].credit_note_id, notes[1].credit_note_id);
        assert_eq!(apps[1].amount_cents, 100);
        assert_eq!(apps[1].remaining_balance_cents, 400);
        assert!(!apps[1].consumed());
    }

    #[test]
    fn partial_application_keeps_the_rest_of_the_balance() {
        let notes = [note(1_000)];
        let apps = allocate_credit_notes(250, &notes);
        assert_eq!(apps, vec![CreditNoteApplication {
            credit_note_id: notes[0].credit_note_id,
            amount_cents: 250,
            remaining_balance_cents: 750,
        }]);
    }

    #[test]
    fn stops_once_the_invoice_is_covered() {
        let notes = [note(500), note(500)];
        let apps = allocate_credit_notes(500, &notes);
        assert_eq!(apps.len(), 1);
        assert!(apps[0].consumed());
    }

    #[test]
    fn nothing_due_or_no_balance_applies_nothing() {
        assert!(allocate_credit_notes(0, &[note(100)]).is_empty());
        assert!(allocate_credit_notes(-5, &[note(100)]).is_empty());
        let apps = allocate_credit_notes(100, &[note(0), note(-3), note(40)]);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].amount_cents, 40);
    }

    #[test]
    fn total_applied_never_exceeds_due_or_balances() {
        for due in [0_i64, 1, 99, 100, 101, 250, 10_000] {
            let notes = [note(37), note(63), note(150)];
            let apps = allocate_credit_notes(due, &notes);
            let applied: i64 = apps.iter().map(|a| a.amount_cents).sum();
            assert_eq!(applied, due.min(250));
            for (a, n) in apps.iter().zip(notes.iter()) {
                assert_eq!(a.amount_cents + a.remaining_balance_cents, n.balance_amount_cents);
            }
        }
    }

    #[test]
    fn applications_reduce_the_invoice_total() {
        use rago_core::enums::{InvoiceStatus, InvoiceType, PaymentStatus};
        let now = chrono::Utc::now();
        let mut invoice = Invoice {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            billing_entity_id: Uuid::new_v4(),
            customer_id: Some(Uuid::new_v4()),
            number: "INV-1".into(),
            sequential_id: Some(1),
            invoice_type: InvoiceType::Subscription,
            status: InvoiceStatus::Finalized,
            payment_status: PaymentStatus::Pending,
            currency: Some("USD".into()),
            issuing_date: Some(now.date_naive()),
            fees_amount_cents: 1_000,
            coupons_amount_cents: 0,
            credit_notes_amount_cents: 0,
            prepaid_credit_amount_cents: 0,
            progressive_billing_credit_amount_cents: 0,
            sub_total_excluding_taxes_amount_cents: 1_000,
            sub_total_including_taxes_amount_cents: 1_200,
            taxes_amount_cents: 200,
            taxes_rate: 20.0,
            total_amount_cents: 1_200,
            version_number: 4,
            created_at: now,
            updated_at: now,
            applied_taxes: Vec::new(),
        };
        let apps = allocate_credit_notes(invoice.total_amount_cents, &[note(500)]);
        apply_credit_note_applications(&mut invoice, &apps);
        assert_eq!(invoice.credit_notes_amount_cents, 500);
        assert_eq!(invoice.total_amount_cents, 700);
        assert!(matches!(invoice.payment_status, PaymentStatus::Pending));

        let apps = allocate_credit_notes(invoice.total_amount_cents, &[note(900)]);
        apply_credit_note_applications(&mut invoice, &apps);
        assert_eq!(invoice.credit_notes_amount_cents, 1_200);
        assert_eq!(invoice.total_amount_cents, 0);
        assert!(matches!(invoice.payment_status, PaymentStatus::Succeeded));
    }
}
