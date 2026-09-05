//! Progressive billing + true-ups.
//!
//! Progressive billing bills usage *mid-period* as usage thresholds are crossed
//! (see [`crate::usage_thresholds`]); a "progressive billing" invoice
//! (`invoice_type: progressive_billing`) is issued for the usage so far. At the
//! end of the period the regular subscription invoice is generated for the whole
//! period's usage, and the amounts already billed progressively must be credited
//! back so the customer is not billed twice.
//!
//! This module ports the credit-back leg:
//!
//! * `Subscriptions::ProgressiveBilledAmount` — how much of a progressive
//!   invoice is still creditable (`to_credit_amount`).
//! * `Credits::ProgressiveBillingService` — applies that credit to the final
//!   invoice: it discounts the matching charge fees, and when the progressively
//!   billed amount exceeds what the final invoice charges for those same charges,
//!   the excess is credited via a credit note
//!   (`CreditNotes::CreateFromProgressiveBillingInvoice`) — the **true-up**.

use rago_core::models::{Credit, Fee, Invoice};
use serde_json::Value;
use uuid::Uuid;

/// One charge fee on a progressive-billing invoice.
#[derive(Debug, Clone)]
pub struct ProgressiveFee {
    pub charge_id: Uuid,
    pub charge_filter_id: Option<Uuid>,
    pub grouped_by: Value,
    pub amount_cents: i64,
    /// `Fee#creditable_amount_cents` — how much of this fee can still be credited
    /// via a credit note.
    pub creditable_amount_cents: i64,
}

/// A finalized progressive-billing invoice, as consumed by the credit-back.
#[derive(Debug, Clone)]
pub struct ProgressiveBillingInvoice {
    pub id: Uuid,
    pub fees_amount_cents: i64,
    pub coupons_amount_cents: i64,
    /// Active `progressive_billing_credits` already drawn from this invoice.
    pub progressive_billing_credits_amount_cents: i64,
    /// Credit notes already issued against this invoice (available + consumed).
    pub credit_notes_amount_cents: i64,
    pub fees: Vec<ProgressiveFee>,
}

impl ProgressiveBillingInvoice {
    /// `Subscriptions::ProgressiveBilledAmount#to_credit_amount`:
    /// `fees_amount - coupons - progressive_billing_credits - credit_notes`,
    /// floored at zero.
    pub fn to_credit_amount(&self) -> i64 {
        let v = self.fees_amount_cents
            - self.coupons_amount_cents
            - self.progressive_billing_credits_amount_cents
            - self.credit_notes_amount_cents;
        v.max(0)
    }

    fn charge_ids(&self) -> Vec<Uuid> {
        self.fees.iter().map(|f| f.charge_id).collect()
    }
}

/// A credit note the true-up must create against a progressive invoice for the
/// over-credited amount (`CreditNotes::CreateFromProgressiveBillingInvoice`).
#[derive(Debug, Clone, PartialEq)]
pub struct ProgressiveCreditNote {
    pub progressive_billing_invoice_id: Uuid,
    pub amount_cents: i64,
}

/// The outcome of applying one progressive invoice's credit to the final invoice.
pub struct ProgressiveCreditResult {
    /// The `Credit` row created against the final invoice (if any).
    pub credit: Option<Credit>,
    /// The true-up credit note to issue against the progressive invoice for the
    /// portion that exceeded the final invoice's matching charges (if any).
    pub credit_note: Option<ProgressiveCreditNote>,
}

/// `Credits::ProgressiveBillingService#call` for a single subscription's
/// progressive invoice.
///
/// Mutates the final `invoice` and its `fees`:
/// * discounts each matching charge fee by the progressively-billed amount
///   (`precise_coupons_amount_cents`, capped at the fee amount),
/// * reduces `sub_total_excluding_taxes_amount_cents` and increments
///   `progressive_billing_credit_amount_cents` by the credited amount.
///
/// When the amount to credit exceeds what the final invoice bills for those same
/// charges, the excess becomes a **true-up credit note** against the progressive
/// invoice, and the on-invoice credit is capped at the matching charges total.
pub fn apply_progressive_billing_credit(
    invoice: &mut Invoice,
    fees: &mut [Fee],
    pbi: &ProgressiveBillingInvoice,
) -> ProgressiveCreditResult {
    let charge_ids = pbi.charge_ids();

    // total_charges_amount: final-invoice charge fees for the same charges.
    let total_charges_amount: i64 = fees
        .iter()
        .filter(|f| f.is_charge())
        .filter(|f| {
            f.charge_id
                .map(|c| charge_ids.contains(&c))
                .unwrap_or(false)
        })
        .map(|f| f.amount_cents)
        .sum();

    let mut amount_to_credit = pbi.to_credit_amount();
    let mut credit_note = None;

    if amount_to_credit > total_charges_amount {
        credit_note = Some(ProgressiveCreditNote {
            progressive_billing_invoice_id: pbi.id,
            amount_cents: amount_to_credit - total_charges_amount,
        });
        amount_to_credit = total_charges_amount;
    }

    if amount_to_credit <= 0 {
        return ProgressiveCreditResult {
            credit: None,
            credit_note,
        };
    }

    apply_credit_to_fees(fees, pbi);

    invoice.sub_total_excluding_taxes_amount_cents -= amount_to_credit;
    invoice.progressive_billing_credit_amount_cents += amount_to_credit;

    let now = chrono::Utc::now();
    let credit = Credit {
        id: Uuid::new_v4(),
        organization_id: invoice.organization_id,
        invoice_id: Some(invoice.id),
        applied_coupon_id: None,
        credit_note_id: None,
        progressive_billing_invoice_id: Some(pbi.id),
        amount_cents: amount_to_credit,
        amount_currency: invoice.currency().code(),
        before_taxes: true,
        created_at: now,
        updated_at: now,
    };

    ProgressiveCreditResult {
        credit: Some(credit),
        credit_note,
    }
}

/// `Credits::ProgressiveBillingService#apply_credit_to_fees` — for each
/// progressive charge fee, find the matching final-invoice fee (same charge,
/// charge filter and grouping) and add the progressive amount to its coupon
/// share, capped at the fee's own amount.
fn apply_credit_to_fees(fees: &mut [Fee], pbi: &ProgressiveBillingInvoice) {
    for pf in &pbi.fees {
        if let Some(fee) = fees.iter_mut().find(|f| {
            f.is_charge()
                && f.charge_id == Some(pf.charge_id)
                && f.charge_filter_id == pf.charge_filter_id
                && f.grouped_by == pf.grouped_by
        }) {
            fee.precise_coupons_amount_cents += rust_decimal::Decimal::from(pf.amount_cents);
            let amount = rust_decimal::Decimal::from(fee.amount_cents);
            if amount < fee.precise_coupons_amount_cents {
                fee.precise_coupons_amount_cents = amount;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use rago_core::enums::{FeeType, InvoiceStatus, InvoiceType, PaymentStatus};
    use rust_decimal::Decimal;
    use serde_json::json;

    fn invoice() -> Invoice {
        let now = Utc::now();
        Invoice {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            billing_entity_id: Uuid::new_v4(),
            customer_id: Some(Uuid::new_v4()),
            number: "INV-1".into(),
            sequential_id: Some(1),
            invoice_type: InvoiceType::Subscription,
            status: InvoiceStatus::Draft,
            payment_status: PaymentStatus::Pending,
            currency: Some("USD".into()),
            issuing_date: None,
            fees_amount_cents: 1000,
            coupons_amount_cents: 0,
            credit_notes_amount_cents: 0,
            prepaid_credit_amount_cents: 0,
            progressive_billing_credit_amount_cents: 0,
            sub_total_excluding_taxes_amount_cents: 1000,
            sub_total_including_taxes_amount_cents: 0,
            taxes_amount_cents: 0,
            taxes_rate: 0.0,
            total_amount_cents: 0,
            version_number: 1,
            created_at: now,
            updated_at: now,
            applied_taxes: Vec::new(),
        }
    }

    fn charge_fee(charge_id: Uuid, amount_cents: i64) -> Fee {
        let now = Utc::now();
        Fee {
            id: Uuid::new_v4(),
            organization_id: Uuid::new_v4(),
            billing_entity_id: Uuid::new_v4(),
            invoice_id: None,
            subscription_id: Some(Uuid::new_v4()),
            charge_id: Some(charge_id),
            charge_filter_id: None,
            fee_type: Some(FeeType::Charge),
            invoiceable_type: Some("Charge".into()),
            invoiceable_id: Some(charge_id),
            amount_cents,
            amount_currency: "USD".into(),
            precise_amount_cents: Decimal::from(amount_cents),
            unit_amount_cents: amount_cents,
            precise_unit_amount: Decimal::ZERO,
            taxes_amount_cents: 0,
            taxes_precise_amount_cents: Decimal::ZERO,
            taxes_rate: 0.0,
            units: Decimal::ONE,
            total_aggregated_units: Some(Decimal::ONE),
            events_count: Some(0),
            precise_coupons_amount_cents: Decimal::ZERO,
            precise_credit_notes_amount_cents: Decimal::ZERO,
            payment_status: PaymentStatus::Pending,
            properties: json!({}),
            amount_details: json!({}),
            grouped_by: json!({}),
            created_at: now,
            updated_at: now,
            applied_taxes: Vec::new(),
        }
    }

    // ---- Lago spec: credits/progressive_billing_service_spec.rb ----

    #[test]
    fn applies_one_credit_of_20() {
        // Two charge fees (500, 500); the progressive invoice has a single fee of
        // 20 on fee1's charge. → credit 20, fee1 coupon share 20, fee2 0.
        let charge1 = Uuid::new_v4();
        let charge2 = Uuid::new_v4();
        let mut inv = invoice();
        let mut fees = vec![charge_fee(charge1, 500), charge_fee(charge2, 500)];
        let pbi = ProgressiveBillingInvoice {
            id: Uuid::new_v4(),
            fees_amount_cents: 20,
            coupons_amount_cents: 0,
            progressive_billing_credits_amount_cents: 0,
            credit_notes_amount_cents: 0,
            fees: vec![ProgressiveFee {
                charge_id: charge1,
                charge_filter_id: None,
                grouped_by: json!({}),
                amount_cents: 20,
                creditable_amount_cents: 20,
            }],
        };

        let res = apply_progressive_billing_credit(&mut inv, &mut fees, &pbi);
        let credit = res.credit.expect("credit");
        assert_eq!(credit.amount_cents, 20);
        assert_eq!(credit.progressive_billing_invoice_id, Some(pbi.id));
        assert!(res.credit_note.is_none());
        assert_eq!(inv.progressive_billing_credit_amount_cents, 20);
        assert_eq!(inv.sub_total_excluding_taxes_amount_cents, 1000 - 20);
        assert_eq!(fees[0].precise_coupons_amount_cents, Decimal::from(20));
        assert_eq!(fees[1].precise_coupons_amount_cents, Decimal::ZERO);
    }

    #[test]
    fn credit_capped_at_fee_amount_when_greater() {
        // fee1 amount 19 < progressive 20: credit capped at total charges (19),
        // fee1 coupon share capped at 19, and a 1-cent true-up credit note.
        let charge1 = Uuid::new_v4();
        let mut inv = invoice();
        let mut fees = vec![charge_fee(charge1, 19)];
        let pbi = ProgressiveBillingInvoice {
            id: Uuid::new_v4(),
            fees_amount_cents: 20,
            coupons_amount_cents: 0,
            progressive_billing_credits_amount_cents: 0,
            credit_notes_amount_cents: 0,
            fees: vec![ProgressiveFee {
                charge_id: charge1,
                charge_filter_id: None,
                grouped_by: json!({}),
                amount_cents: 20,
                creditable_amount_cents: 20,
            }],
        };

        let res = apply_progressive_billing_credit(&mut inv, &mut fees, &pbi);
        let credit = res.credit.expect("credit");
        assert_eq!(credit.amount_cents, 19);
        assert_eq!(inv.progressive_billing_credit_amount_cents, 19);
        assert_eq!(fees[0].precise_coupons_amount_cents, Decimal::from(19));
        assert_eq!(
            res.credit_note,
            Some(ProgressiveCreditNote {
                progressive_billing_invoice_id: pbi.id,
                amount_cents: 1
            })
        );
    }

    #[test]
    fn additional_non_matching_fee_not_discounted() {
        // fees: charge1 300, charge2 300, plus a non-charge fee (400).
        // Progressive fee (20) only on charge1. Credit 20 → fee1 coupon 20.
        let charge1 = Uuid::new_v4();
        let charge2 = Uuid::new_v4();
        let mut inv = invoice();
        let mut fee3 = charge_fee(Uuid::new_v4(), 400);
        fee3.fee_type = Some(FeeType::Subscription); // non-charge
        fee3.charge_id = None;
        let mut fees = vec![charge_fee(charge1, 300), charge_fee(charge2, 300), fee3];
        let pbi = ProgressiveBillingInvoice {
            id: Uuid::new_v4(),
            fees_amount_cents: 20,
            coupons_amount_cents: 0,
            progressive_billing_credits_amount_cents: 0,
            credit_notes_amount_cents: 0,
            fees: vec![ProgressiveFee {
                charge_id: charge1,
                charge_filter_id: None,
                grouped_by: json!({}),
                amount_cents: 20,
                creditable_amount_cents: 20,
            }],
        };

        let res = apply_progressive_billing_credit(&mut inv, &mut fees, &pbi);
        assert_eq!(res.credit.unwrap().amount_cents, 20);
        assert_eq!(inv.progressive_billing_credit_amount_cents, 20);
        assert_eq!(fees[0].precise_coupons_amount_cents, Decimal::from(20));
        assert_eq!(fees[1].precise_coupons_amount_cents, Decimal::ZERO);
        assert_eq!(fees[2].precise_coupons_amount_cents, Decimal::ZERO);
    }

    #[test]
    fn no_progressive_invoice_no_credit() {
        let mut inv = invoice();
        let mut fees = vec![charge_fee(Uuid::new_v4(), 500)];
        let pbi = ProgressiveBillingInvoice {
            id: Uuid::new_v4(),
            fees_amount_cents: 0,
            coupons_amount_cents: 0,
            progressive_billing_credits_amount_cents: 0,
            credit_notes_amount_cents: 0,
            fees: vec![],
        };
        let res = apply_progressive_billing_credit(&mut inv, &mut fees, &pbi);
        assert!(res.credit.is_none());
        assert_eq!(inv.progressive_billing_credit_amount_cents, 0);
    }

    #[test]
    fn to_credit_amount_subtracts_coupons_and_prior_credits() {
        let pbi = ProgressiveBillingInvoice {
            id: Uuid::new_v4(),
            fees_amount_cents: 100,
            coupons_amount_cents: 10,
            progressive_billing_credits_amount_cents: 20,
            credit_notes_amount_cents: 5,
            fees: vec![],
        };
        assert_eq!(pbi.to_credit_amount(), 65);

        // Never negative.
        let pbi2 = ProgressiveBillingInvoice {
            credit_notes_amount_cents: 1000,
            ..pbi
        };
        assert_eq!(pbi2.to_credit_amount(), 0);
    }
}
