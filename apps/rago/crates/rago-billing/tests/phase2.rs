//! Phase 2 proofs: the remaining charge models + aggregators, subscription
//! date boundaries, the subscription fee, pay-in-advance delta pricing, and a
//! full subscription→invoice generation. Every expected value is hand-derived
//! from the corresponding Lago Ruby service.

use chrono::{NaiveDate, NaiveDateTime, NaiveTime, Utc};
use rago_billing::aggregation::{self, AggregationResult};
use rago_billing::charge_models::{self, PricingStructure};
use rago_billing::charges::apply_pay_in_advance;
use rago_billing::dates::MonthlyDates;
use rago_billing::generate::{generate_subscription_invoice, ChargeInput};
use rago_billing::subscription_fee::{build_subscription_fee, Proration};
use rago_core::enums::{
    AggregationType, ChargeModel, InvoiceStatus, InvoiceType, PaymentStatus, PlanInterval,
    SubscriptionStatus,
};
use rago_core::models::{BillableMetric, Charge, Event, Invoice, Plan, Subscription, Tax};
use rago_core::money::Currency;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::{json, Value};
use uuid::Uuid;

fn now() -> chrono::DateTime<Utc> {
    Utc::now()
}
fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}
fn dt(y: i32, m: u32, day: u32) -> NaiveDateTime {
    NaiveDateTime::new(d(y, m, day), NaiveTime::from_hms_opt(0, 0, 0).unwrap())
}

fn metric(agg: AggregationType, field: &str) -> BillableMetric {
    BillableMetric {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        name: "m".into(),
        code: "m".into(),
        description: None,
        aggregation_type: agg,
        field_name: Some(field.into()),
        recurring: false,
        expression: None,
        rounding_function: None,
        rounding_precision: None,
        custom_aggregator: None,
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
        code: "c".into(),
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

fn events_num(field: &str, values: &[i64]) -> Vec<Event> {
    values.iter().map(|v| ev(json!({ field: v }))).collect()
}
fn ev(props: Value) -> Event {
    Event {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        customer_id: None,
        subscription_id: None,
        transaction_id: Uuid::new_v4().to_string(),
        code: "m".into(),
        properties: props,
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

fn ps(model: ChargeModel, props: Value) -> PricingStructure {
    PricingStructure {
        charge_model: model,
        properties: props,
        prorated: false,
        accepts_target_wallet: false,
        currency: Currency::new("USD"),
    }
}

// ---------------------------------------------------------------- charge models

#[test]
fn graduated_percentage_model() {
    // [0..10] rate 10% flat 5 ; [11..∞] rate 5% flat 2. units = 30.
    //   tier1: 10 units * 10% = 1, + flat 5 = 6.
    //   tier2: 20 units * 5%  = 1, + flat 2 = 3.  total = 9.
    let props = json!({
        "graduated_percentage_ranges": [
            { "from_value": 0, "to_value": 10, "rate": "10", "flat_amount": "5" },
            { "from_value": 11, "to_value": null, "rate": "5", "flat_amount": "2" }
        ]
    });
    let agg = aggregation::aggregate(
        &metric(AggregationType::SumAgg, "value"),
        &events_num("value", &[30]),
        0,
        Decimal::ZERO,
    )
    .unwrap();
    let cmr = charge_models::compute(&ps(ChargeModel::GraduatedPercentage, props), &agg).unwrap();
    assert_eq!(cmr.amount, dec!(9));
}

#[test]
fn dynamic_model() {
    // precise_total_amount_cents = 12345 → 12345 / 100 = 123.45.
    let agg = AggregationResult {
        aggregation: dec!(50),
        precise_total_amount_cents: Some(dec!(12345)),
        ..Default::default()
    };
    let cmr = charge_models::compute(&ps(ChargeModel::Dynamic, json!({})), &agg).unwrap();
    assert_eq!(cmr.amount, dec!(123.45));
    assert_eq!(cmr.unit_amount, dec!(123.45) / dec!(50));
}

#[test]
fn custom_model() {
    let agg = AggregationResult {
        aggregation: dec!(5),
        custom_amount: Some(dec!(42.50)),
        ..Default::default()
    };
    let cmr = charge_models::compute(&ps(ChargeModel::Custom, json!({})), &agg).unwrap();
    assert_eq!(cmr.amount, dec!(42.50));
}

#[test]
fn prorated_graduated_full_coefficient_matches_graduated() {
    // Single event, prorated == full (active whole period).
    // Ranges [0..10] @1 ; [11..∞] @0.5. 30 units ⇒ 10*1 + 20*0.5 = 20.
    let props = json!({
        "graduated_ranges": [
            { "from_value": 0, "to_value": 10, "per_unit_amount": "1", "flat_amount": "0" },
            { "from_value": 11, "to_value": null, "per_unit_amount": "0.5", "flat_amount": "0" }
        ]
    });
    let agg = AggregationResult {
        aggregation: dec!(30),
        event_full_values: vec![dec!(30)],
        event_prorated_values: vec![dec!(30)],
        ..Default::default()
    };
    let mut structure = ps(ChargeModel::Graduated, props);
    structure.prorated = true;
    let cmr = charge_models::compute(&structure, &agg).unwrap();
    assert_eq!(cmr.amount, dec!(20));
}

#[test]
fn prorated_graduated_half_period() {
    // Same ranges, but the 30-unit event was active half the period ⇒
    // prorated 15. tier1 prices 5 units @1 = 5, tier2 10 units @0.5 = 5 ⇒ 10.
    let props = json!({
        "graduated_ranges": [
            { "from_value": 0, "to_value": 10, "per_unit_amount": "1", "flat_amount": "0" },
            { "from_value": 11, "to_value": null, "per_unit_amount": "0.5", "flat_amount": "0" }
        ]
    });
    let agg = AggregationResult {
        aggregation: dec!(15),
        event_full_values: vec![dec!(30)],
        event_prorated_values: vec![dec!(15)],
        ..Default::default()
    };
    let mut structure = ps(ChargeModel::Graduated, props);
    structure.prorated = true;
    let cmr = charge_models::compute(&structure, &agg).unwrap();
    assert_eq!(cmr.amount, dec!(10));
}

#[test]
fn grouped_charge_model() {
    // Two groups priced with the standard model @0.5: 100u→50, 40u→20.
    let group = |units: Decimal, region: &str| {
        let mut gb = std::collections::BTreeMap::new();
        gb.insert("region".to_string(), Some(region.to_string()));
        AggregationResult {
            aggregation: units,
            current_usage_units: Some(units),
            grouped_by: gb,
            ..Default::default()
        }
    };
    let agg = AggregationResult {
        aggregations: vec![group(dec!(100), "us"), group(dec!(40), "eu")],
        ..Default::default()
    };
    let props = json!({ "amount": "0.5", "pricing_group_keys": ["region"] });
    let cmr = charge_models::compute(&ps(ChargeModel::Standard, props), &agg).unwrap();
    assert_eq!(cmr.amount, dec!(70.0));
    assert_eq!(cmr.units, dec!(140));
}

// ---------------------------------------------------------------- aggregators

#[test]
fn max_latest_unique_aggregators() {
    let evs = events_num("value", &[10, 50, 30]);
    let max = aggregation::aggregate(
        &metric(AggregationType::MaxAgg, "value"),
        &evs,
        0,
        Decimal::ZERO,
    )
    .unwrap();
    assert_eq!(max.aggregation, dec!(50));

    let latest = aggregation::aggregate(
        &metric(AggregationType::LatestAgg, "value"),
        &evs,
        0,
        Decimal::ZERO,
    )
    .unwrap();
    assert_eq!(latest.aggregation, dec!(30));

    let uniq_events = vec![
        ev(json!({"value": "a"})),
        ev(json!({"value": "b"})),
        ev(json!({"value": "a"})),
        ev(json!({"value": "c"})),
    ];
    let uniq = aggregation::aggregate(
        &metric(AggregationType::UniqueCountAgg, "value"),
        &uniq_events,
        0,
        Decimal::ZERO,
    )
    .unwrap();
    assert_eq!(uniq.aggregation, dec!(3));
}

#[test]
fn weighted_sum_aggregator() {
    // Period 2 days. +10 at day0, +5 at day1, measured to day2.
    //   day0→day1: running 10, 1 day ⇒ 10 * 86400/172800 = 5.
    //   day1→day2: running 15, 1 day ⇒ 15 * 86400/172800 = 7.5.  total 12.5.
    let events = [(dt(2024, 1, 1), dec!(10)), (dt(2024, 1, 2), dec!(5))];
    let r = aggregation::weighted_sum(&events, dt(2024, 1, 1), dt(2024, 1, 3), 2, Decimal::ZERO);
    assert_eq!(r.aggregation, dec!(12.5));
    assert_eq!(r.variation, Some(dec!(15)));
    assert_eq!(r.count, 2);
}

#[test]
fn rounding_functions() {
    let mut m = metric(AggregationType::SumAgg, "value");
    m.rounding_function = Some("ceil".into());
    m.rounding_precision = Some(0);
    // sum [1.2, 2.3] = 3.5 → ceil(0) = 4.
    let evs = vec![ev(json!({"value": 1.2})), ev(json!({"value": 2.3}))];
    let r = aggregation::aggregate(&m, &evs, 0, Decimal::ZERO).unwrap();
    assert_eq!(r.aggregation, dec!(4));

    m.rounding_function = Some("floor".into());
    let r2 = aggregation::aggregate(&m, &evs, 0, Decimal::ZERO).unwrap();
    assert_eq!(r2.aggregation, dec!(3));
}

// ---------------------------------------------------------------- prorated sum

#[test]
fn prorated_sum_aggregation() {
    // Period 30 days. Event value 30 on day 16 ⇒ active (30-16+1)=15 days ⇒
    // coefficient 15/30 = 0.5 ⇒ prorated 15.
    let events = [(d(2024, 4, 16), dec!(30))];
    let r = aggregation::prorated_sum(&events, d(2024, 4, 30), 30);
    assert_eq!(r.aggregation, dec!(15));
    assert_eq!(r.event_full_values, vec![dec!(30)]);
    assert_eq!(r.event_prorated_values, vec![dec!(15)]);
}

// ---------------------------------------------------------------- dates

#[test]
fn monthly_calendar_boundaries() {
    let dates = MonthlyDates {
        subscription_at: d(2024, 1, 1),
        billing_at: d(2024, 2, 1),
        anniversary: false,
        pay_in_advance: false,
        current_usage: false,
    };
    let b = dates.boundaries();
    assert_eq!(b.from_date, d(2024, 1, 1));
    assert_eq!(b.to_date, d(2024, 1, 31));
    assert_eq!(b.charges_duration_days, 31);
}

#[test]
fn monthly_anniversary_boundaries() {
    let dates = MonthlyDates {
        subscription_at: d(2024, 1, 15),
        billing_at: d(2024, 2, 15),
        anniversary: true,
        pay_in_advance: false,
        current_usage: false,
    };
    let b = dates.boundaries();
    assert_eq!(b.from_date, d(2024, 1, 15));
    assert_eq!(b.to_date, d(2024, 2, 14));
    assert_eq!(b.charges_duration_days, 31);
}

// ---------------------------------------------------------------- subscription fee

fn plan_usd(amount_cents: i64) -> Plan {
    Plan {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        name: "p".into(),
        code: "p".into(),
        interval: Some(PlanInterval::Monthly),
        description: None,
        amount_cents: Some(amount_cents),
        amount_currency: "USD".into(),
        trial_period: None,
        pay_in_advance: false,
        bill_charges_monthly: None,
        parent_id: None,
        invoice_display_name: None,
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

fn jan_dates() -> MonthlyDates {
    MonthlyDates {
        subscription_at: d(2024, 1, 1),
        billing_at: d(2024, 2, 1),
        anniversary: false,
        pay_in_advance: false,
        current_usage: false,
    }
}

#[test]
fn subscription_fee_full_period() {
    let plan = plan_usd(10000);
    let sub = subscription();
    let fee = build_subscription_fee(
        &plan,
        &sub,
        None,
        sub.organization_id,
        &jan_dates(),
        Proration::Full,
    );
    assert_eq!(fee.amount_cents, 10000);
    assert_eq!(fee.units, dec!(1));
}

#[test]
fn subscription_fee_prorated() {
    // 10 of 31 days: 10 * (10000/31) = 100000/31 = 3225.806… → round 3226.
    let plan = plan_usd(10000);
    let sub = subscription();
    let fee = build_subscription_fee(
        &plan,
        &sub,
        None,
        sub.organization_id,
        &jan_dates(),
        Proration::Prorated { days_to_bill: 10 },
    );
    assert_eq!(fee.amount_cents, 3226);
}

// ---------------------------------------------------------------- pay in advance

#[test]
fn pay_in_advance_standard() {
    // Standard @2.0. Event added 3 units to a period total of 10.
    //   cost(10) - cost(7) = 20 - 14 = 6.00 → 600c, units 3, unit 2.
    let mut structure = ps(ChargeModel::Standard, json!({ "amount": "2" }));
    structure.prorated = false;
    let agg = AggregationResult {
        aggregation: dec!(10),
        count: 4,
        pay_in_advance_aggregation: Some(dec!(3)),
        pay_in_advance_event_persisted: true,
        ..Default::default()
    };
    let r = apply_pay_in_advance(&structure, &agg);
    assert_eq!(r.amount_cents_i64(), 600);
    assert_eq!(r.units, dec!(3));
    assert_eq!(r.unit_amount, dec!(2));
}

#[test]
fn pay_in_advance_graduated_delta() {
    // Graduated [0..10]@1 ; [11..∞]@0.5. Event took the period 10→12.
    //   cost(12) = 10 + 2*0.5 = 11 ; cost(10) = 10 ; delta = 1.00 → 100c.
    let props = json!({
        "graduated_ranges": [
            { "from_value": 0, "to_value": 10, "per_unit_amount": "1", "flat_amount": "0" },
            { "from_value": 11, "to_value": null, "per_unit_amount": "0.5", "flat_amount": "0" }
        ]
    });
    let structure = ps(ChargeModel::Graduated, props);
    let agg = AggregationResult {
        aggregation: dec!(12),
        count: 5,
        pay_in_advance_aggregation: Some(dec!(2)),
        pay_in_advance_event_persisted: true,
        ..Default::default()
    };
    let r = apply_pay_in_advance(&structure, &agg);
    assert_eq!(r.amount_cents_i64(), 100);
    assert_eq!(r.units, dec!(2));
}

// ---------------------------------------------------------------- full invoice

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

#[test]
fn full_subscription_invoice_generation() {
    // Plan $100.00 (10000c) + one standard usage charge @0.5 on 100 units =
    // $50.00 (5000c). VAT 20% on 15000 = 3000. Total 18000.
    let plan = plan_usd(10000);
    let sub = subscription();
    let metric = metric(AggregationType::SumAgg, "value");
    let ch = charge(ChargeModel::Standard, json!({ "amount": "0.5" }));
    let evs = events_num("value", &[100]);

    let mut invoice = blank_invoice();
    let charges = [ChargeInput {
        filters: &[],
        persisted_events: &[],
        metric: &metric,
        charge: &ch,
        events: &evs,
        period: None,
    }];
    let fees = generate_subscription_invoice(
        &mut invoice,
        &plan,
        &sub,
        &jan_dates(),
        Proration::Full,
        &charges,
        &[tax(20.0)],
        &[],
    )
    .unwrap();

    assert_eq!(fees.len(), 2); // subscription fee + charge fee
    assert_eq!(fees[0].amount_cents, 10000); // subscription
    assert_eq!(fees[1].amount_cents, 5000); // charge
    assert_eq!(invoice.fees_amount_cents, 15000);
    assert_eq!(invoice.sub_total_excluding_taxes_amount_cents, 15000);
    assert_eq!(invoice.taxes_amount_cents, 3000);
    assert_eq!(invoice.total_amount_cents, 18000);
    assert_eq!(invoice.payment_status, PaymentStatus::Pending);
}
