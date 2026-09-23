//! Phase 3 proofs: wallet credit↔amount + balance math, an invoice combining a
//! coupon (before tax) and prepaid wallet credit (after tax) in the correct
//! order, and a credit note created from an invoice. Numbers hand-derived from
//! the Lago Ruby services.

use chrono::Utc;
use rago_billing::charge_to_fee;
use rago_billing::credit_notes::{create_credit_note, CreditRefund};
use rago_billing::invoices::{applied_coupon_after, assemble_invoice, CouponApplication};
use rago_billing::prepaid::apply_prepaid_credit;
use rago_billing::wallets::{build_transaction, decrease_balance, increase_balance, WalletCredit};
use rago_core::enums::{
    ChargeModel, CouponFrequency, CouponStatus, CouponType, InvoiceStatus, InvoiceType,
    PaymentStatus, WalletStatus, WalletTransactionStatus, WalletTransactionType,
};
use rago_core::models::{
    AppliedCoupon, BillableMetric, Charge, Coupon, CreditNoteItem, Fee, Invoice, Subscription, Tax,
    Wallet,
};
use rago_core::money::Currency;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::json;
use uuid::Uuid;

fn now() -> chrono::DateTime<Utc> {
    Utc::now()
}

fn wallet(rate: Decimal) -> Wallet {
    Wallet {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        customer_id: Uuid::new_v4(),
        status: WalletStatus::Active,
        name: None,
        rate_amount: rate,
        credits_balance: Decimal::ZERO,
        consumed_credits: Decimal::ZERO,
        balance_cents: 0,
        balance_currency: "USD".into(),
        consumed_amount_cents: 0,
        ongoing_balance_cents: 0,
        ongoing_usage_balance_cents: 0,
        credits_ongoing_balance: Decimal::ZERO,
        credits_ongoing_usage_balance: Decimal::ZERO,
        expiration_at: None,
        terminated_at: None,
        lock_version: 0,
        priority: 50,
        created_at: now(),
        updated_at: now(),
    }
}

// ---------------------------------------------------------------- wallets

#[test]
fn wallet_purchase_and_conversion() {
    // rate 1.0: buy 50 credits → $50.00 → 5000c, 50 credits.
    let w = wallet(dec!(1));
    let credit =
        WalletCredit::from_credit_amount(Currency::new("USD"), w.rate_amount, dec!(50), true);
    assert_eq!(credit.amount_cents, 5000);
    assert_eq!(credit.credit_amount, dec!(50));
    let tx = build_transaction(
        &w,
        &credit,
        WalletTransactionType::Inbound,
        WalletTransactionStatus::Settled,
        None,
    );
    let w = increase_balance(&w, &tx, false);
    assert_eq!(w.balance_cents, 5000);
    assert_eq!(w.credits_balance, dec!(50));
}

#[test]
fn wallet_conversion_with_rate() {
    // rate 0.5 ($0.50/credit): buy 100 credits → $50.00 → 5000c, 100 credits.
    let w = wallet(dec!(0.5));
    let credit =
        WalletCredit::from_credit_amount(Currency::new("USD"), w.rate_amount, dec!(100), true);
    assert_eq!(credit.amount_cents, 5000);
    assert_eq!(credit.credit_amount, dec!(100));
    let tx = build_transaction(
        &w,
        &credit,
        WalletTransactionType::Inbound,
        WalletTransactionStatus::Settled,
        None,
    );
    let w = increase_balance(&w, &tx, false);
    assert_eq!(w.balance_cents, 5000);
    assert_eq!(w.credits_balance, dec!(100));

    // Consume $20.00 (2000c) → 40 credits at rate 0.5.
    let spend = WalletCredit::from_amount_cents(Currency::new("USD"), w.rate_amount, 2000);
    assert_eq!(spend.credit_amount, dec!(40));
    let tx2 = build_transaction(
        &w,
        &spend,
        WalletTransactionType::Outbound,
        WalletTransactionStatus::Settled,
        None,
    );
    let w = decrease_balance(&w, &tx2);
    assert_eq!(w.balance_cents, 3000);
    assert_eq!(w.credits_balance, dec!(60));
    assert_eq!(w.consumed_credits, dec!(40));
    assert_eq!(w.consumed_amount_cents, 2000);
}

// ---------------------------------------------------------------- shared builders

fn metric() -> BillableMetric {
    BillableMetric {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        name: "m".into(),
        code: "m".into(),
        description: None,
        aggregation_type: rago_core::enums::AggregationType::SumAgg,
        field_name: Some("value".into()),
        recurring: false,
        expression: None,
        rounding_function: None,
        rounding_precision: None,
        created_at: now(),
        updated_at: now(),
        deleted_at: None,
    }
}

fn charge_std(amount: &str) -> Charge {
    Charge {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        billable_metric_id: None,
        plan_id: None,
        code: "c".into(),
        charge_model: ChargeModel::Standard,
        properties: json!({ "amount": amount }),
        pay_in_advance: false,
        prorated: false,
        invoiceable: true,
        min_amount_cents: 0,
        amount_currency: Some("USD".into()),
        accepts_target_wallet: false,
        invoice_display_name: None,
        parent_id: None,
        created_at: now(),
        updated_at: now(),
        deleted_at: None,
    }
}

fn subscription() -> Subscription {
    let org = Uuid::new_v4();
    Subscription {
        id: Uuid::new_v4(),
        organization_id: org,
        customer_id: Uuid::new_v4(),
        plan_id: Uuid::new_v4(),
        billing_entity_id: Some(org),
        external_id: "s".into(),
        name: None,
        status: rago_core::enums::SubscriptionStatus::Active,
        billing_time: rago_core::enums::BillingTime::Calendar,
        subscription_at: None,
        started_at: None,
        ending_at: None,
        trial_ended_at: None,
        canceled_at: None,
        terminated_at: None,
        previous_subscription_id: None,
        created_at: now(),
        updated_at: now(),
    }
}

fn ev(value: i64) -> rago_core::models::Event {
    rago_core::models::Event {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        customer_id: None,
        subscription_id: None,
        transaction_id: Uuid::new_v4().to_string(),
        code: "m".into(),
        properties: json!({ "value": value }),
        external_customer_id: None,
        external_subscription_id: None,
        timestamp: None,
        precise_total_amount_cents: None,
        created_at: now(),
        updated_at: now(),
        deleted_at: None,
        persisted: true,
    }
}

fn tax(rate: f64) -> Tax {
    Tax {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        name: "VAT".into(),
        code: "vat".into(),
        description: None,
        rate,
        applied_to_organization: true,
        created_at: now(),
        updated_at: now(),
    }
}

fn blank_invoice() -> Invoice {
    Invoice {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        billing_entity_id: Uuid::nil(),
        customer_id: Some(Uuid::new_v4()),
        number: "INV-1".into(),
        sequential_id: Some(1),
        invoice_type: InvoiceType::Subscription,
        status: InvoiceStatus::Finalized,
        payment_status: PaymentStatus::Pending,
        currency: Some("USD".into()),
        issuing_date: None,
        fees_amount_cents: 0,
        coupons_amount_cents: 0,
        credit_notes_amount_cents: 0,
        prepaid_credit_amount_cents: 0,
        progressive_billing_credit_amount_cents: 0,
        sub_total_excluding_taxes_amount_cents: 0,
        sub_total_including_taxes_amount_cents: 0,
        taxes_amount_cents: 0,
        taxes_rate: 0.0,
        total_amount_cents: 0,
        version_number: 4,
        created_at: now(),
        updated_at: now(),
        applied_taxes: Vec::new(),
    }
}

fn hundred_dollar_fee() -> Fee {
    // standard @1.0 * 100 units = $100.00 (10000c).
    let m = metric();
    let ch = charge_std("1");
    let evs = vec![ev(100)];
    let mut fee =
        charge_to_fee(&m, &ch, &subscription(), &evs, Currency::new("USD"), None).unwrap();
    fee.organization_id = Uuid::nil();
    fee
}

// ---------------------------------------------------------------- coupon + prepaid ordering

#[test]
fn invoice_coupon_before_tax_then_prepaid_after_tax() {
    // Fee $100 (10000c). 10% coupon (before tax) → 1000c off, sub-total 9000c.
    // VAT 20% on 9000 = 1800c → total 10800c. Then prepaid wallet ($50 = 5000c)
    // after tax → total 5800c, prepaid_credit 5000c, wallet balance 0.
    let coupon = Coupon {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        name: "10%".into(),
        code: "ten".into(),
        status: CouponStatus::Active,
        coupon_type: CouponType::Percentage,
        amount_cents: None,
        amount_currency: None,
        percentage_rate: Some(dec!(10)),
        frequency: CouponFrequency::Once,
        frequency_duration: None,
        expiration: rago_core::enums::CouponExpiration::NoExpiration,
        expiration_at: None,
        reusable: true,
        limited_plans: false,
        limited_billable_metrics: false,
        created_at: now(),
        updated_at: now(),
        deleted_at: None,
    };
    let applied = AppliedCoupon {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        coupon_id: coupon.id,
        customer_id: Uuid::new_v4(),
        status: CouponStatus::Active,
        amount_cents: None,
        amount_currency: None,
        percentage_rate: Some(dec!(10)),
        frequency: CouponFrequency::Once,
        frequency_duration: None,
        frequency_duration_remaining: None,
        created_at: now(),
        updated_at: now(),
        terminated_at: None,
    };

    let mut invoice = blank_invoice();
    let mut fees = vec![hundred_dollar_fee()];
    let coupon_apps = vec![CouponApplication::new(&applied, &coupon)];
    assemble_invoice(&mut invoice, &mut fees, &[tax(20.0)], &coupon_apps);

    assert_eq!(invoice.coupons_amount_cents, 1000);
    assert_eq!(invoice.sub_total_excluding_taxes_amount_cents, 9000);
    assert_eq!(invoice.taxes_amount_cents, 1800);
    assert_eq!(invoice.total_amount_cents, 10800);

    // Prepaid wallet credit ($50) applied after tax.
    let w = {
        let mut w = wallet(dec!(1));
        w.balance_cents = 5000;
        w.credits_balance = dec!(50);
        w
    };
    let res = apply_prepaid_credit(&mut invoice, &fees, &[w]);
    assert_eq!(res.prepaid_credit_amount_cents, 5000);
    assert_eq!(invoice.prepaid_credit_amount_cents, 5000);
    assert_eq!(invoice.total_amount_cents, 5800);
    assert_eq!(res.updated_wallets[0].balance_cents, 0);

    // The once coupon is terminated after use.
    let after = applied_coupon_after(&coupon_apps[0], Decimal::from(1000));
    assert_eq!(after.status, CouponStatus::Terminated);
}

#[test]
fn recurring_coupon_decrements_duration() {
    let coupon = Coupon {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        name: "$10".into(),
        code: "ten".into(),
        status: CouponStatus::Active,
        coupon_type: CouponType::FixedAmount,
        amount_cents: Some(1000),
        amount_currency: Some("USD".into()),
        percentage_rate: None,
        frequency: CouponFrequency::Recurring,
        frequency_duration: Some(3),
        expiration: rago_core::enums::CouponExpiration::NoExpiration,
        expiration_at: None,
        reusable: true,
        limited_plans: false,
        limited_billable_metrics: false,
        created_at: now(),
        updated_at: now(),
        deleted_at: None,
    };
    let applied = AppliedCoupon {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        coupon_id: coupon.id,
        customer_id: Uuid::new_v4(),
        status: CouponStatus::Active,
        amount_cents: Some(1000),
        amount_currency: Some("USD".into()),
        percentage_rate: None,
        frequency: CouponFrequency::Recurring,
        frequency_duration: Some(3),
        frequency_duration_remaining: Some(3),
        created_at: now(),
        updated_at: now(),
        terminated_at: None,
    };
    let app = CouponApplication::new(&applied, &coupon);
    let after = applied_coupon_after(&app, Decimal::from(1000));
    assert_eq!(after.frequency_duration_remaining, Some(2));
    assert_eq!(after.status, CouponStatus::Active); // still 2 left
}

// ---------------------------------------------------------------- credit notes

#[test]
fn credit_note_full_fee_with_tax() {
    // Finalized invoice: fee $100 (10000c) + VAT 20% = 2000c, total 12000c.
    let mut invoice = blank_invoice();
    let mut fees = vec![hundred_dollar_fee()];
    assemble_invoice(&mut invoice, &mut fees, &[tax(20.0)], &[]);
    assert_eq!(invoice.total_amount_cents, 12000);

    // Credit the whole fee: item 10000c ⇒ credit = 10000 − 0 + 2000 = 12000.
    let item = CreditNoteItem {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        credit_note_id: None,
        fee_id: fees[0].id,
        amount_cents: 10000,
        precise_amount_cents: dec!(10000),
        amount_currency: "USD".into(),
    };
    let cn = create_credit_note(&invoice, &fees, vec![item], CreditRefund::CreditOnly, 1);
    assert_eq!(cn.taxes_amount_cents, 2000);
    assert_eq!(cn.credit_amount_cents, 12000);
    assert_eq!(cn.total_amount_cents, 12000);
    assert_eq!(cn.balance_amount_cents, 12000);
    assert_eq!(cn.refund_amount_cents, 0);
    assert_eq!(cn.taxes_rate, 20.0);
}

#[test]
fn credit_note_partial_with_refund_split() {
    // Half the fee: item 5000c ⇒ base 5000, tax 1000, creditable 6000.
    // Split: refund 4000, credit 2000.
    let mut invoice = blank_invoice();
    let mut fees = vec![hundred_dollar_fee()];
    assemble_invoice(&mut invoice, &mut fees, &[tax(20.0)], &[]);

    let item = CreditNoteItem {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        credit_note_id: None,
        fee_id: fees[0].id,
        amount_cents: 5000,
        precise_amount_cents: dec!(5000),
        amount_currency: "USD".into(),
    };
    let cn = create_credit_note(
        &invoice,
        &fees,
        vec![item],
        CreditRefund::Split {
            refund_amount_cents: 4000,
        },
        1,
    );
    assert_eq!(cn.taxes_amount_cents, 1000);
    assert_eq!(cn.total_amount_cents, 6000);
    assert_eq!(cn.refund_amount_cents, 4000);
    assert_eq!(cn.credit_amount_cents, 2000);
    assert_eq!(cn.balance_amount_cents, 2000);
}

#[test]
fn credit_note_with_coupon_adjustment() {
    // Fee $100 with a $10 (1000c) coupon applied ⇒ taxable base 9000.
    // Full-fee item 10000c ⇒ coupons_adjustment 1000, tax 1800,
    // creditable = 10000 − 1000 + 1800 = 10800.
    let coupon = Coupon {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        name: "$10".into(),
        code: "ten".into(),
        status: CouponStatus::Active,
        coupon_type: CouponType::FixedAmount,
        amount_cents: Some(1000),
        amount_currency: Some("USD".into()),
        percentage_rate: None,
        frequency: CouponFrequency::Once,
        frequency_duration: None,
        expiration: rago_core::enums::CouponExpiration::NoExpiration,
        expiration_at: None,
        reusable: true,
        limited_plans: false,
        limited_billable_metrics: false,
        created_at: now(),
        updated_at: now(),
        deleted_at: None,
    };
    let applied = AppliedCoupon {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        coupon_id: coupon.id,
        customer_id: Uuid::new_v4(),
        status: CouponStatus::Active,
        amount_cents: Some(1000),
        amount_currency: Some("USD".into()),
        percentage_rate: None,
        frequency: CouponFrequency::Once,
        frequency_duration: None,
        frequency_duration_remaining: None,
        created_at: now(),
        updated_at: now(),
        terminated_at: None,
    };

    let mut invoice = blank_invoice();
    let mut fees = vec![hundred_dollar_fee()];
    let coupon_apps = vec![CouponApplication::new(&applied, &coupon)];
    assemble_invoice(&mut invoice, &mut fees, &[tax(20.0)], &coupon_apps);
    // sub-total 9000, tax 1800, total 10800.
    assert_eq!(invoice.total_amount_cents, 10800);

    let item = CreditNoteItem {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        credit_note_id: None,
        fee_id: fees[0].id,
        amount_cents: 10000,
        precise_amount_cents: dec!(10000),
        amount_currency: "USD".into(),
    };
    let cn = create_credit_note(&invoice, &fees, vec![item], CreditRefund::CreditOnly, 1);
    assert_eq!(cn.coupons_adjustment_amount_cents, 1000);
    assert_eq!(cn.taxes_amount_cents, 1800);
    assert_eq!(cn.credit_amount_cents, 10800);
}

#[test]
fn provider_taxes_applied_to_fee() {
    use rago_billing::fees::{apply_provider_taxes, ProviderTaxLine};
    // A $50.00 (5000c) fee, external tax 8.5% → 425c.
    let m = metric();
    let ch = charge_std("0.5");
    let evs = vec![ev(100)];
    let mut fee =
        charge_to_fee(&m, &ch, &subscription(), &evs, Currency::new("USD"), None).unwrap();
    apply_provider_taxes(
        &mut fee,
        &[ProviderTaxLine {
            name: "CA State Tax".into(),
            rate: 0.085,
            tax_amount: 425,
            type_: "sales_tax".into(),
        }],
    );
    assert_eq!(fee.taxes_amount_cents, 425);
    assert_eq!(fee.taxes_rate, 8.5);
    assert_eq!(fee.applied_taxes.len(), 1);
    assert_eq!(fee.applied_taxes[0].tax_name, "CA State Tax");
}
