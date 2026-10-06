//! Charge filters, grouped pricing and prorated charges are billed
//! (`Fees::ChargeService#init_fees`), not just stored.
//!
//! Before: every billing path priced a charge over all of the subscription's
//! events at the charge's base price, so a filtered plan billed EU traffic at
//! the default rate, grouped pricing was one summed line and a prorated seat
//! charge billed full seats for days the seats did not exist.

use chrono::{Duration, NaiveDate, TimeZone, Utc};
use rago_billing::aggregation::AggregationPeriod;
use rago_billing::charge_fees::{buckets, charge_fees, ChargeUsage, FilterSpec};
use rago_billing::dates::MonthlyDates;
use rago_billing::generate::{generate_subscription_invoice, ChargeInput};
use rago_billing::subscription_fee::Proration;
use rago_core::enums::{
    AggregationType, BillingTime, ChargeModel, InvoiceStatus, InvoiceType, PaymentStatus, PlanInterval,
    SubscriptionStatus,
};
use rago_core::models::{BillableMetric, Charge, Event, Invoice, Plan, Subscription};
use rago_core::money::Currency;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::{json, Value};
use uuid::Uuid;

fn usd() -> Currency {
    Currency::new("USD")
}

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn metric(agg: AggregationType, field: Option<&str>, recurring: bool) -> BillableMetric {
    let now = Utc::now();
    BillableMetric {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        name: "m".into(),
        code: "m".into(),
        description: None,
        aggregation_type: agg,
        field_name: field.map(str::to_string),
        recurring,
        expression: None,
        rounding_function: None,
        rounding_precision: None,
        custom_aggregator: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn charge(m: &BillableMetric, model: ChargeModel, properties: Value) -> Charge {
    let now = Utc::now();
    Charge {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        billable_metric_id: Some(m.id),
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
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn sub() -> Subscription {
    let now = Utc::now();
    Subscription {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        customer_id: Uuid::new_v4(),
        plan_id: Uuid::new_v4(),
        billing_entity_id: None,
        external_id: "ext".into(),
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

fn ev(day: NaiveDate, props: Value) -> Event {
    let now = Utc::now();
    Event {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        customer_id: None,
        subscription_id: None,
        transaction_id: Uuid::new_v4().to_string(),
        code: "m".into(),
        properties: props,
        external_customer_id: None,
        external_subscription_id: Some("ext".into()),
        timestamp: Some(day.and_hms_opt(12, 0, 0).unwrap()),
        precise_total_amount_cents: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        persisted: true,
    }
}

fn filter(properties: Value, values: Value, age_secs: i64) -> FilterSpec {
    let created = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap() + Duration::seconds(age_secs);
    FilterSpec::from_parts(Uuid::new_v4(), properties, &values, created)
}

fn june() -> AggregationPeriod {
    AggregationPeriod::new(
        d(2026, 6, 1).and_hms_opt(0, 0, 0).unwrap(),
        d(2026, 6, 30).and_hms_opt(23, 59, 59).unwrap(),
    )
}

fn usage<'a>(
    m: &'a BillableMetric,
    c: &'a Charge,
    filters: &'a [FilterSpec],
    events: &'a [Event],
    persisted: &'a [Event],
    period: &'a AggregationPeriod,
) -> ChargeUsage<'a> {
    ChargeUsage {
        metric: m,
        charge: c,
        filters,
        events,
        persisted_events: persisted,
        period: Some(period),
    }
}

#[test]
fn each_filter_bills_its_own_events_at_its_own_price_and_the_rest_at_the_default() {
    let m = metric(AggregationType::SumAgg, Some("gb"), false);
    let c = charge(&m, ChargeModel::Standard, json!({ "amount": "1" }));
    let eu = filter(json!({ "amount": "3" }), json!({ "region": ["eu"] }), 0);
    let us = filter(json!({ "amount": "2" }), json!({ "region": ["us"] }), 1);
    let filters = [eu.clone(), us.clone()];
    let events = [
        ev(d(2026, 6, 3), json!({ "gb": 3, "region": "eu" })),
        ev(d(2026, 6, 4), json!({ "gb": 2, "region": "us" })),
        ev(d(2026, 6, 5), json!({ "gb": 5, "region": "asia" })),
        ev(d(2026, 6, 6), json!({ "gb": 1 })),
    ];
    let p = june();
    let fees = charge_fees(&usage(&m, &c, &filters, &events, &[], &p), &sub(), usd(), None).unwrap();
    let by = |id: Option<Uuid>| fees.iter().find(|f| f.charge_filter_id == id).unwrap();
    assert_eq!(fees.len(), 3);
    assert_eq!((by(Some(eu.id)).units, by(Some(eu.id)).amount_cents), (dec!(3), 900));
    assert_eq!((by(Some(us.id)).units, by(Some(us.id)).amount_cents), (dec!(2), 400));
    // asia (5) and the event without a region (1) are the default bucket.
    assert_eq!((by(None).units, by(None).amount_cents), (dec!(6), 600));
    // Before this fix one fee billed all 11 GB at the default $1.
    assert_eq!(fees.iter().map(|f| f.amount_cents).sum::<i64>(), 1_900);
}

#[test]
fn an_event_in_a_more_specific_filter_is_counted_once() {
    let m = metric(AggregationType::CountAgg, None, false);
    let c = charge(&m, ChargeModel::Standard, json!({ "amount": "1" }));
    let eu = filter(json!({ "amount": "2" }), json!({ "region": ["eu"] }), 0);
    let eu_gold = filter(
        json!({ "amount": "5" }),
        json!({ "region": ["eu"], "tier": ["gold"] }),
        1,
    );
    let filters = [eu.clone(), eu_gold.clone()];
    let events = [
        ev(d(2026, 6, 3), json!({ "region": "eu", "tier": "gold" })),
        ev(d(2026, 6, 3), json!({ "region": "eu", "tier": "free" })),
        ev(d(2026, 6, 3), json!({ "region": "eu" })),
    ];
    let p = june();
    let fees = charge_fees(&usage(&m, &c, &filters, &events, &[], &p), &sub(), usd(), None).unwrap();
    let units = |id: Uuid| {
        fees.iter()
            .find(|f| f.charge_filter_id == Some(id))
            .map(|f| f.units)
            .unwrap()
    };
    assert_eq!(units(eu_gold.id), dec!(1));
    assert_eq!(units(eu.id), dec!(2));
    assert!(
        fees.iter().all(|f| f.charge_filter_id.is_some()),
        "nothing falls to the default bucket"
    );
    assert_eq!(fees.iter().map(|f| f.amount_cents).sum::<i64>(), 900);
}

#[test]
fn of_two_identical_filters_only_the_oldest_counts() {
    let m = metric(AggregationType::CountAgg, None, false);
    let c = charge(&m, ChargeModel::Standard, json!({ "amount": "1" }));
    let older = filter(json!({ "amount": "2" }), json!({ "region": ["eu"] }), 0);
    let newer = filter(json!({ "amount": "9" }), json!({ "region": ["eu"] }), 10);
    let b = buckets(&c, &[newer.clone(), older.clone()]);
    let events = [ev(d(2026, 6, 3), json!({ "region": "eu" }))];
    let in_bucket = |id: Uuid| b.iter().find(|x| x.filter_id == Some(id)).unwrap().contains(&events[0]);
    assert!(in_bucket(older.id));
    assert!(!in_bucket(newer.id));
}

#[test]
fn a_sibling_with_overlapping_values_loses_only_the_shared_values() {
    // Lago: when a child has the same keys but is not a subset, the filter's
    // own values are removed from the child before it is ignored.
    let m = metric(AggregationType::CountAgg, None, false);
    let c = charge(&m, ChargeModel::Standard, json!({ "amount": "1" }));
    let a = filter(json!({ "amount": "2" }), json!({ "region": ["eu"] }), 0);
    let b = filter(json!({ "amount": "3" }), json!({ "region": ["eu", "us"] }), 1);
    let events = [
        ev(d(2026, 6, 3), json!({ "region": "eu" })),
        ev(d(2026, 6, 3), json!({ "region": "us" })),
    ];
    let filters = [a.clone(), b.clone()];
    let p = june();
    let fees = charge_fees(&usage(&m, &c, &filters, &events, &[], &p), &sub(), usd(), None).unwrap();
    let units = |id: Uuid| {
        fees.iter()
            .find(|f| f.charge_filter_id == Some(id))
            .map(|f| f.units)
            .unwrap_or_default()
    };
    // a: eu; b: eu and us minus a's eu = us.
    assert_eq!(units(a.id), dec!(1));
    assert_eq!(units(b.id), dec!(1));
}

#[test]
fn grouped_pricing_bills_one_fee_per_group_with_its_group_values() {
    let m = metric(AggregationType::SumAgg, Some("tokens"), false);
    let c = charge(
        &m,
        ChargeModel::Graduated,
        json!({
            "pricing_group_keys": ["workspace"],
            "graduated_ranges": [
                { "from_value": 0, "to_value": 10, "per_unit_amount": "1", "flat_amount": "0" },
                { "from_value": 11, "to_value": null, "per_unit_amount": "0.5", "flat_amount": "0" }
            ]
        }),
    );
    let events = [
        ev(d(2026, 6, 2), json!({ "tokens": 8, "workspace": "a" })),
        ev(d(2026, 6, 2), json!({ "tokens": 8, "workspace": "b" })),
        ev(d(2026, 6, 3), json!({ "tokens": 8, "workspace": "a" })),
    ];
    let p = june();
    let fees = charge_fees(&usage(&m, &c, &[], &events, &[], &p), &sub(), usd(), None).unwrap();
    assert_eq!(fees.len(), 2);
    let a = fees
        .iter()
        .find(|f| f.grouped_by == json!({ "workspace": "a" }))
        .unwrap();
    let b = fees
        .iter()
        .find(|f| f.grouped_by == json!({ "workspace": "b" }))
        .unwrap();
    // Each group runs the graduated ranges from zero: a = 10*1 + 6*0.5 = 13, b = 8.
    assert_eq!((a.units, a.amount_cents), (dec!(16), 1_300));
    assert_eq!((b.units, b.amount_cents), (dec!(8), 800));
}

#[test]
fn a_filter_groups_by_its_own_keys() {
    let m = metric(AggregationType::CountAgg, None, false);
    let c = charge(&m, ChargeModel::Standard, json!({ "amount": "1" }));
    let eu = filter(
        json!({ "amount": "2", "pricing_group_keys": ["user"] }),
        json!({ "region": ["eu"] }),
        0,
    );
    let events = [
        ev(d(2026, 6, 3), json!({ "region": "eu", "user": "u1" })),
        ev(d(2026, 6, 3), json!({ "region": "eu", "user": "u2" })),
        ev(d(2026, 6, 3), json!({ "region": "eu", "user": "u2" })),
    ];
    let filters = [eu.clone()];
    let p = june();
    let fees = charge_fees(&usage(&m, &c, &filters, &events, &[], &p), &sub(), usd(), None).unwrap();
    let mut got: Vec<(Value, i64)> = fees
        .iter()
        .filter(|f| f.charge_filter_id == Some(eu.id))
        .map(|f| (f.grouped_by.clone(), f.amount_cents))
        .collect();
    got.sort_by_key(|(g, _)| g.to_string());
    assert_eq!(
        got,
        vec![(json!({ "user": "u1" }), 200), (json!({ "user": "u2" }), 400)]
    );
    // The default bucket had no event: no zero line next to real ones.
    assert!(fees.iter().all(|f| f.charge_filter_id.is_some()));
}

#[test]
fn a_prorated_seat_charge_bills_seats_for_the_days_they_existed() {
    // 10 seats carried in from May (recurring), 30 more added on June 16:
    // 10 for 30/30 days + 30 for 15/30 days = 25 seat-months; units show 40.
    let mut m = metric(AggregationType::SumAgg, Some("seats"), true);
    m.recurring = true;
    let mut c = charge(&m, ChargeModel::Standard, json!({ "amount": "10" }));
    c.prorated = true;
    let persisted = [ev(d(2026, 5, 10), json!({ "seats": 10 }))];
    let events = [ev(d(2026, 6, 16), json!({ "seats": 30 }))];
    let p = june();
    let fees = charge_fees(&usage(&m, &c, &[], &events, &persisted, &p), &sub(), usd(), None).unwrap();
    assert_eq!(fees.len(), 1);
    assert_eq!(fees[0].units, dec!(40));
    assert_eq!(fees[0].amount_cents, 25_000);
    // Without proration the same charge bills 40 full seats.
    c.prorated = false;
    let full = charge_fees(&usage(&m, &c, &[], &events, &persisted, &p), &sub(), usd(), None).unwrap();
    assert_eq!(
        full[0].amount_cents, 30_000,
        "non-prorated reads only the period's events"
    );
}

#[test]
fn prorated_and_grouped_together() {
    let m = metric(AggregationType::SumAgg, Some("seats"), true);
    let mut c = charge(
        &m,
        ChargeModel::Standard,
        json!({ "amount": "3", "pricing_group_keys": ["team"] }),
    );
    c.prorated = true;
    let persisted = [ev(d(2026, 5, 2), json!({ "seats": 2, "team": "a" }))];
    let events = [
        ev(d(2026, 6, 21), json!({ "seats": 3, "team": "a" })), // 10/30 days
        ev(d(2026, 6, 1), json!({ "seats": 1, "team": "b" })),  // 30/30 days
    ];
    let p = june();
    let fees = charge_fees(&usage(&m, &c, &[], &events, &persisted, &p), &sub(), usd(), None).unwrap();
    let a = fees.iter().find(|f| f.grouped_by == json!({ "team": "a" })).unwrap();
    let b = fees.iter().find(|f| f.grouped_by == json!({ "team": "b" })).unwrap();
    // a: 2 + 3*10/30 = 3 seat-months * $3 = $9; b: 1 * $3 = $3.
    assert_eq!((a.units, a.amount_cents), (dec!(5), 900));
    assert_eq!((b.units, b.amount_cents), (dec!(1), 300));
}

#[test]
fn a_prorated_unique_count_bills_each_member_for_their_days() {
    let m = metric(AggregationType::UniqueCountAgg, Some("member"), true);
    let mut c = charge(&m, ChargeModel::Standard, json!({ "amount": "30" }));
    c.prorated = true;
    // alice since May, bob joins June 16 (15 days of 30).
    let persisted = [ev(d(2026, 5, 3), json!({ "member": "alice" }))];
    let events = [ev(d(2026, 6, 16), json!({ "member": "bob" }))];
    let p = june();
    let fees = charge_fees(&usage(&m, &c, &[], &events, &persisted, &p), &sub(), usd(), None).unwrap();
    assert_eq!(fees[0].units, dec!(2));
    assert_eq!(fees[0].amount_cents, 4_500);
}

#[test]
fn a_charge_without_filters_or_groups_keeps_its_single_fee_even_at_zero() {
    let m = metric(AggregationType::CountAgg, None, false);
    let c = charge(&m, ChargeModel::Standard, json!({ "amount": "1" }));
    let p = june();
    let fees = charge_fees(&usage(&m, &c, &[], &[], &[], &p), &sub(), usd(), None).unwrap();
    assert_eq!(fees.len(), 1);
    assert_eq!(fees[0].amount_cents, 0);
    assert_eq!(fees[0].charge_filter_id, None);
}

fn plan() -> Plan {
    let now = Utc::now();
    Plan {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        name: "p".into(),
        code: "p".into(),
        interval: Some(PlanInterval::Monthly),
        description: None,
        amount_cents: Some(0),
        amount_currency: "USD".into(),
        trial_period: None,
        pay_in_advance: false,
        bill_charges_monthly: None,
        parent_id: None,
        invoice_display_name: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn blank_invoice(_s: &Subscription) -> Invoice {
    let now = Utc::now();
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
        created_at: now,
        updated_at: now,
        applied_taxes: Vec::new(),
    }
}

#[test]
fn the_invoice_sums_every_filter_fee_and_a_minimum_trues_up_the_whole_charge() {
    let m = metric(AggregationType::SumAgg, Some("gb"), false);
    let mut c = charge(&m, ChargeModel::Standard, json!({ "amount": "1" }));
    c.min_amount_cents = 2_000;
    let eu = filter(json!({ "amount": "3" }), json!({ "region": ["eu"] }), 0);
    let filters = [eu.clone()];
    let events = [
        ev(d(2026, 6, 3), json!({ "gb": 3, "region": "eu" })),
        ev(d(2026, 6, 5), json!({ "gb": 5, "region": "asia" })),
    ];
    let mut s = sub();
    s.started_at = Some(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
    let mut invoice = blank_invoice(&s);
    let dates = MonthlyDates {
        subscription_at: d(2026, 1, 1),
        billing_at: d(2026, 7, 1),
        anniversary: false,
        pay_in_advance: false,
        current_usage: false,
    };
    let p = june();
    let inputs = [ChargeInput {
        metric: &m,
        charge: &c,
        filters: &filters,
        events: &events,
        persisted_events: &[],
        period: Some(p),
    }];
    let fees =
        generate_subscription_invoice(&mut invoice, &plan(), &s, &dates, Proration::Full, &inputs, &[], &[]).unwrap();
    let charge_fees: Vec<_> = fees.iter().filter(|f| f.charge_id == Some(c.id)).collect();
    // eu 3*3 = 900, default 5*1 = 500, true-up 2000 - 1400 = 600 on the default fee.
    let total: i64 = charge_fees.iter().map(|f| f.amount_cents).sum();
    assert_eq!(total, 2_000);
    let true_up = charge_fees
        .iter()
        .find(|f| f.properties.get("true_up_parent_fee_id").is_some())
        .unwrap();
    assert_eq!(true_up.amount_cents, 600);
    let parent = true_up.properties["true_up_parent_fee_id"].as_str().unwrap();
    let parent = charge_fees.iter().find(|f| f.id.to_string() == parent).unwrap();
    assert_eq!(parent.charge_filter_id, None);
    assert_eq!(invoice.fees_amount_cents, 2_000);
    assert_eq!(Decimal::from(invoice.fees_amount_cents), dec!(2000));
}
