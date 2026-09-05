//! End-to-end billing vertical proof.
//!
//! Drives the real ported services — aggregation → charge model → fee →
//! invoice — and asserts the exact numbers Lago's Ruby produces. Each expected
//! value is hand-derived from the corresponding `app/services/*` code.

use chrono::Utc;
use rago_billing::aggregation;
use rago_billing::charge_models::{self, PricingStructure};
use rago_billing::invoices::{assemble_invoice, CouponApplication};
use rago_billing::{charge_to_fee, fees};
use rago_core::enums::{
    AggregationType, ChargeModel, CouponFrequency, CouponStatus, CouponType, InvoiceStatus,
    InvoiceType, PaymentStatus, SubscriptionStatus,
};
use rago_core::models::{
    AppliedCoupon, BillableMetric, Charge, Coupon, Event, Invoice, Subscription, Tax,
};
use rago_core::money::Currency;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::{json, Value};
use uuid::Uuid;

fn now() -> chrono::DateTime<Utc> {
    Utc::now()
}

fn sum_metric() -> BillableMetric {
    BillableMetric {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        name: "API calls".into(),
        code: "api_calls".into(),
        description: None,
        aggregation_type: AggregationType::SumAgg,
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

fn charge(model: ChargeModel, properties: Value) -> Charge {
    Charge {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        billable_metric_id: None,
        plan_id: None,
        code: "charge".into(),
        charge_model: model,
        properties,
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
        external_id: "sub_1".into(),
        name: None,
        status: SubscriptionStatus::Active,
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

/// Build `n` events each carrying `{ "value": v }`.
fn events(values: &[i64]) -> Vec<Event> {
    values
        .iter()
        .map(|v| Event {
            id: Uuid::new_v4(),
            organization_id: Uuid::nil(),
            customer_id: None,
            subscription_id: None,
            transaction_id: Uuid::new_v4().to_string(),
            code: "api_calls".into(),
            properties: json!({ "value": v }),
            external_customer_id: None,
            external_subscription_id: None,
            timestamp: None,
            precise_total_amount_cents: None,
            created_at: now(),
            updated_at: now(),
            deleted_at: None,
            persisted: true,
        })
        .collect()
}

// -------------------------------------------------------------------------
// Charge-model math
// -------------------------------------------------------------------------

#[test]
fn standard_charge_model() {
    // sum_agg over [40, 35, 25] = 100 units. amount = 100 * 0.5 = 50.00.
    let metric = sum_metric();
    let ch = charge(ChargeModel::Standard, json!({ "amount": "0.5" }));
    let evs = events(&[40, 35, 25]);

    let agg = aggregation::aggregate(&metric, &evs, 0, Decimal::ZERO).unwrap();
    assert_eq!(agg.aggregation, dec!(100));
    assert_eq!(agg.count, 3);

    let ps = PricingStructure::from_charge(&ch, Currency::new("USD"));
    let cmr = charge_models::compute(&ps, &agg).unwrap();
    assert_eq!(cmr.amount, dec!(50.0)); // 100 * 0.5
    assert_eq!(cmr.unit_amount, dec!(0.5)); // 50 / 100

    // Fee: amount_cents = round(50.00, 2) * 100 = 5000.
    let fee = charge_to_fee(
        &metric,
        &ch,
        &subscription(),
        &evs,
        Currency::new("USD"),
        None,
    )
    .unwrap();
    assert_eq!(fee.amount_cents, 5000);
    assert_eq!(fee.precise_amount_cents, dec!(5000.0));
    assert_eq!(fee.unit_amount_cents, 50);
    assert_eq!(fee.units, dec!(100));
    assert_eq!(fee.events_count, Some(3));
}

#[test]
fn graduated_charge_model_non_adjacent() {
    // Ranges: [0..10] per_unit 1, flat 5 ; [11..∞] per_unit 0.5, flat 0.
    // Non-adjacent (11 != 10) ⇒ tier-2 units = (30 - 11) + 1 = 20.
    //   tier1: 10 * 1 + 5 = 15 ; tier2: 20 * 0.5 + 0 = 10 ⇒ total 25.00.
    let props = json!({
        "graduated_ranges": [
            { "from_value": 0, "to_value": 10, "per_unit_amount": "1", "flat_amount": "5" },
            { "from_value": 11, "to_value": null, "per_unit_amount": "0.5", "flat_amount": "0" }
        ]
    });
    let metric = sum_metric();
    let ch = charge(ChargeModel::Graduated, props);
    let evs = events(&[30]);

    let agg = aggregation::aggregate(&metric, &evs, 0, Decimal::ZERO).unwrap();
    let ps = PricingStructure::from_charge(&ch, Currency::new("USD"));
    let cmr = charge_models::compute(&ps, &agg).unwrap();
    assert_eq!(cmr.amount, dec!(25));

    let ranges = cmr.amount_details["graduated_ranges"].as_array().unwrap();
    assert_eq!(ranges.len(), 2);
    assert_eq!(ranges[0]["total_with_flat_amount"], "15"); // 10*1 + 5
    assert_eq!(ranges[1]["total_with_flat_amount"], "10.0"); // 20*0.5 + 0 (BigDecimal scale)

    let fee = charge_to_fee(
        &metric,
        &ch,
        &subscription(),
        &evs,
        Currency::new("USD"),
        None,
    )
    .unwrap();
    assert_eq!(fee.amount_cents, 2500);
}

#[test]
fn graduated_charge_model_adjacent() {
    // Adjacent ranges: [0..10] and [10..∞]. tier-2 units = 30 - 10 = 20.
    //   tier1: 10 * 2 + 0 = 20 ; tier2: 20 * 1 + 10 = 30 ⇒ total 50.00.
    let props = json!({
        "graduated_ranges": [
            { "from_value": 0, "to_value": 10, "per_unit_amount": "2", "flat_amount": "0" },
            { "from_value": 10, "to_value": null, "per_unit_amount": "1", "flat_amount": "10" }
        ]
    });
    let metric = sum_metric();
    let ch = charge(ChargeModel::Graduated, props);
    let evs = events(&[30]);
    let agg = aggregation::aggregate(&metric, &evs, 0, Decimal::ZERO).unwrap();
    let ps = PricingStructure::from_charge(&ch, Currency::new("USD"));
    let cmr = charge_models::compute(&ps, &agg).unwrap();
    assert_eq!(cmr.amount, dec!(50));
}

#[test]
fn package_charge_model() {
    // free_units 10, package_size 100, amount 5. units = 235.
    //   paid = 225 ; packages = ceil(225/100) = 3 ; amount = 3 * 5 = 15.00.
    let props = json!({ "free_units": 10, "package_size": 100, "amount": "5" });
    let metric = sum_metric();
    let ch = charge(ChargeModel::Package, props);
    let evs = events(&[235]);
    let agg = aggregation::aggregate(&metric, &evs, 0, Decimal::ZERO).unwrap();
    let ps = PricingStructure::from_charge(&ch, Currency::new("USD"));
    let cmr = charge_models::compute(&ps, &agg).unwrap();
    assert_eq!(cmr.amount, dec!(15));
}

#[test]
fn volume_charge_model() {
    // Volume ranges: [0..100] per_unit 1 flat 0 ; [101..∞] per_unit 0.5 flat 10.
    // units = 150 ⇒ whole usage priced in tier 2: 150 * 0.5 + 10 = 85.00.
    let props = json!({
        "volume_ranges": [
            { "from_value": 0, "to_value": 100, "per_unit_amount": "1", "flat_amount": "0" },
            { "from_value": 101, "to_value": null, "per_unit_amount": "0.5", "flat_amount": "10" }
        ]
    });
    let metric = sum_metric();
    let ch = charge(ChargeModel::Volume, props);
    let evs = events(&[150]);
    let agg = aggregation::aggregate(&metric, &evs, 0, Decimal::ZERO).unwrap();
    let ps = PricingStructure::from_charge(&ch, Currency::new("USD"));
    let cmr = charge_models::compute(&ps, &agg).unwrap();
    assert_eq!(cmr.amount, dec!(85.0));
}

#[test]
fn percentage_charge_model() {
    // rate 1% + fixed 0.5 per event, no free units. 3 events summing to 10000.
    //   percentage = 10000 * 1 / 100 = 100 ; fixed = 3 * 0.5 = 1.5 ⇒ 101.50.
    let props = json!({ "rate": "1", "fixed_amount": "0.5" });
    let metric = sum_metric();
    let ch = charge(ChargeModel::Percentage, props);
    let evs = events(&[5000, 3000, 2000]);
    let agg = aggregation::aggregate(&metric, &evs, 0, Decimal::ZERO).unwrap();
    let ps = PricingStructure::from_charge(&ch, Currency::new("USD"));
    let cmr = charge_models::compute(&ps, &agg).unwrap();
    assert_eq!(cmr.amount, dec!(101.5));
}

// -------------------------------------------------------------------------
// Full invoice assembly: fee → taxes → invoice totals
// -------------------------------------------------------------------------

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
        customer_id: None,
        number: "INV-1".into(),
        sequential_id: Some(1),
        invoice_type: InvoiceType::Subscription,
        status: InvoiceStatus::Draft,
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

#[test]
fn invoice_with_tax_no_coupon() {
    // One standard fee of $50.00 (5000c), VAT 20% ⇒ taxes 1000c, total 6000c.
    let metric = sum_metric();
    let ch = charge(ChargeModel::Standard, json!({ "amount": "0.5" }));
    let evs = events(&[100]);
    let sub = subscription();
    let fee = charge_to_fee(&metric, &ch, &sub, &evs, Currency::new("USD"), None).unwrap();

    let mut invoice = blank_invoice();
    let mut fees_vec = vec![fee];
    let credits = assemble_invoice(&mut invoice, &mut fees_vec, &[tax(20.0)], &[]);

    assert!(credits.is_empty());
    assert_eq!(invoice.fees_amount_cents, 5000);
    assert_eq!(invoice.sub_total_excluding_taxes_amount_cents, 5000);
    assert_eq!(invoice.taxes_amount_cents, 1000);
    assert_eq!(invoice.sub_total_including_taxes_amount_cents, 6000);
    assert_eq!(invoice.total_amount_cents, 6000);
    assert_eq!(fees_vec[0].taxes_amount_cents, 1000);
}

#[test]
fn invoice_with_percentage_coupon_then_tax() {
    // Fee $50.00 (5000c). 10% coupon applied BEFORE tax ⇒ coupon 500c,
    // sub_total_excl = 4500c, VAT 20% on 4500 = 900c, total 5400c.
    let metric = sum_metric();
    let ch = charge(ChargeModel::Standard, json!({ "amount": "0.5" }));
    let evs = events(&[100]);
    let sub = subscription();
    let mut fee = charge_to_fee(&metric, &ch, &sub, &evs, Currency::new("USD"), None).unwrap();
    fee.organization_id = Uuid::nil();

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
    let mut fees_vec = vec![fee];
    let coupon_apps = vec![CouponApplication::new(&applied, &coupon)];
    let credits = assemble_invoice(&mut invoice, &mut fees_vec, &[tax(20.0)], &coupon_apps);

    assert_eq!(credits.len(), 1);
    assert_eq!(credits[0].amount_cents, 500);
    assert_eq!(invoice.fees_amount_cents, 5000);
    assert_eq!(invoice.coupons_amount_cents, 500);
    assert_eq!(invoice.sub_total_excluding_taxes_amount_cents, 4500);
    assert_eq!(invoice.taxes_amount_cents, 900);
    assert_eq!(invoice.total_amount_cents, 5400);
    // The fee absorbed the whole coupon.
    assert_eq!(fees_vec[0].precise_coupons_amount_cents, dec!(500));
}

#[test]
fn fixed_amount_coupon() {
    // Fee $50.00 (5000c). $12.00 (1200c) fixed once coupon, no tax.
    let metric = sum_metric();
    let ch = charge(ChargeModel::Standard, json!({ "amount": "0.5" }));
    let evs = events(&[100]);
    let sub = subscription();
    let mut fee = charge_to_fee(&metric, &ch, &sub, &evs, Currency::new("USD"), None).unwrap();
    fee.organization_id = Uuid::nil();

    let coupon = Coupon {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        name: "$12".into(),
        code: "twelve".into(),
        status: CouponStatus::Active,
        coupon_type: CouponType::FixedAmount,
        amount_cents: Some(1200),
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
        amount_cents: Some(1200),
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
    let mut fees_vec = vec![fee];
    let coupon_apps = vec![CouponApplication::new(&applied, &coupon)];
    let credits = assemble_invoice(&mut invoice, &mut fees_vec, &[], &coupon_apps);

    assert_eq!(credits[0].amount_cents, 1200);
    assert_eq!(invoice.sub_total_excluding_taxes_amount_cents, 3800);
    assert_eq!(invoice.total_amount_cents, 3800);
}

#[test]
fn apply_taxes_directly_on_fee() {
    // Guard the Fees::ApplyTaxesService port in isolation.
    let metric = sum_metric();
    let ch = charge(ChargeModel::Standard, json!({ "amount": "0.5" }));
    let evs = events(&[100]);
    let mut fee = charge_to_fee(
        &metric,
        &ch,
        &subscription(),
        &evs,
        Currency::new("USD"),
        None,
    )
    .unwrap();
    fees::apply_taxes(&mut fee, &[tax(20.0)]);
    assert_eq!(fee.taxes_amount_cents, 1000);
    assert_eq!(fee.taxes_rate, 20.0);
    assert_eq!(fee.applied_taxes.len(), 1);
}
