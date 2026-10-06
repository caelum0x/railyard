//! Phase 4 proofs: event ingestion→validation→aggregation→billing, the
//! remaining interval date services, subscription termination (prorated final
//! invoice), and a plan upgrade (proration + old-plan credit). Numbers
//! hand-derived from the Lago Ruby services.

use chrono::{NaiveDate, TimeZone, Utc};
use rago_billing::charge_to_fee;
use rago_billing::dates::{Interval, MonthlyDates, PeriodDates};
use rago_billing::events::{
    build_event, numeric_field_for, validate_creation, EventParams, EventStore,
    EventValidationError, InMemoryEventStore,
};
use rago_billing::generate::ChargeInput;
use rago_billing::subscriptions::{date_diff, terminate_subscription, upgrade_subscription};
use rago_core::enums::{
    AggregationType, ChargeModel, InvoiceStatus, InvoiceType, PaymentStatus, PlanInterval,
    SubscriptionStatus,
};
use rago_core::models::{BillableMetric, Charge, Invoice, Plan, Subscription};
use rago_core::money::Currency;
use rust_decimal_macros::dec;
use serde_json::json;
use uuid::Uuid;

fn now() -> chrono::DateTime<Utc> {
    Utc::now()
}
fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}
fn epoch(y: i32, m: u32, day: u32) -> f64 {
    Utc.with_ymd_and_hms(y, m, day, 12, 0, 0)
        .unwrap()
        .timestamp() as f64
}

fn metric() -> BillableMetric {
    BillableMetric {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        name: "api".into(),
        code: "api".into(),
        description: None,
        aggregation_type: AggregationType::SumAgg,
        field_name: Some("value".into()),
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

fn charge_std(amount: &str) -> Charge {
    Charge {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        billable_metric_id: None,
        plan_id: None,
        code: "api".into(),
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

fn subscription(external: &str) -> Subscription {
    let org = Uuid::new_v4();
    Subscription {
        id: Uuid::new_v4(),
        organization_id: org,
        customer_id: Uuid::new_v4(),
        plan_id: Uuid::new_v4(),
        billing_entity_id: Some(org),
        external_id: external.into(),
        name: None,
        status: SubscriptionStatus::Active,
        billing_time: rago_core::enums::BillingTime::Calendar,
        subscription_at: Some(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
        started_at: Some(Utc.with_ymd_and_hms(2024, 1, 1, 0, 0, 0).unwrap()),
        ending_at: None,
        trial_ended_at: None,
        canceled_at: None,
        terminated_at: None,
        previous_subscription_id: None,
        created_at: now(),
        updated_at: now(),
    }
}

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

fn blank_invoice() -> Invoice {
    Invoice {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        billing_entity_id: Uuid::nil(),
        customer_id: Some(Uuid::new_v4()),
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

fn jan_dates() -> MonthlyDates {
    MonthlyDates {
        subscription_at: d(2024, 1, 1),
        billing_at: d(2024, 2, 1),
        anniversary: false,
        pay_in_advance: false,
        current_usage: false,
    }
}

// ---------------------------------------------------------------- ingestion

#[test]
fn event_validation_rules() {
    let m = metric();
    let subs = vec!["sub_1".to_string()];
    let ok = EventParams {
        code: "api".into(),
        transaction_id: "tx1".into(),
        external_subscription_id: "sub_1".into(),
        properties: json!({ "value": 10 }),
        timestamp: Some(epoch(2024, 1, 5)),
        precise_total_amount_cents: None,
    };
    assert!(validate_creation(&ok, &subs, Some(&m), false).is_ok());

    // Unknown subscription.
    let mut bad = ok.clone();
    bad.external_subscription_id = "nope".into();
    assert_eq!(
        validate_creation(&bad, &subs, Some(&m), false),
        Err(EventValidationError::SubscriptionNotFound)
    );

    // Duplicate transaction.
    assert_eq!(
        validate_creation(&ok, &subs, Some(&m), true),
        Err(EventValidationError::TransactionId)
    );

    // Missing metric.
    assert_eq!(
        validate_creation(&ok, &subs, None, false),
        Err(EventValidationError::BillableMetricNotFound)
    );

    // Non-numeric property for a sum metric.
    let mut nan = ok.clone();
    nan.properties = json!({ "value": "abc" });
    assert_eq!(
        validate_creation(&nan, &subs, Some(&m), false),
        Err(EventValidationError::InvalidProperties)
    );
}

#[test]
fn ingest_event_flows_to_fee() {
    // Ingest 3 events (40+35+25 = 100 units) → store → scoped slice →
    // aggregate (sum) → standard charge @0.5 → fee $50.00 (5000c).
    let m = metric();
    let ch = charge_std("0.5");
    let sub = subscription("sub_1");
    let subs = vec!["sub_1".to_string()];
    let mut store = InMemoryEventStore::new();

    for (i, (val, day)) in [(40, 5), (35, 10), (25, 15)].into_iter().enumerate() {
        let params = EventParams {
            code: "api".into(),
            transaction_id: format!("tx{i}"),
            external_subscription_id: "sub_1".into(),
            properties: json!({ "value": val }),
            timestamp: Some(epoch(2024, 1, day)),
            precise_total_amount_cents: None,
        };
        let exists = store.transaction_exists("sub_1", &params.transaction_id);
        validate_creation(&params, &subs, Some(&m), exists).unwrap();
        store.store(build_event(sub.organization_id, &params, now()));
    }

    let from = d(2024, 1, 1).and_hms_opt(0, 0, 0).unwrap();
    let to = d(2024, 1, 31).and_hms_opt(23, 59, 59).unwrap();
    let scoped = store.events("sub_1", "api", Some(from), to, numeric_field_for(&m));
    assert_eq!(scoped.len(), 3);

    let fee = charge_to_fee(&m, &ch, &sub, &scoped, Currency::new("USD"), None).unwrap();
    assert_eq!(fee.units, dec!(100));
    assert_eq!(fee.amount_cents, 5000);

    // An out-of-period event is excluded from the slice.
    let outside = EventParams {
        code: "api".into(),
        transaction_id: "tx_out".into(),
        external_subscription_id: "sub_1".into(),
        properties: json!({ "value": 999 }),
        timestamp: Some(epoch(2024, 2, 5)),
        precise_total_amount_cents: None,
    };
    store.store(build_event(sub.organization_id, &outside, now()));
    let scoped2 = store.events("sub_1", "api", Some(from), to, numeric_field_for(&m));
    assert_eq!(scoped2.len(), 3); // Feb event excluded.
}

// ---------------------------------------------------------------- interval dates

#[test]
fn interval_calendar_durations() {
    let weekly = PeriodDates {
        interval: Interval::Weekly,
        subscription_at: d(2024, 1, 1),
        billing_at: d(2024, 1, 8),
        anniversary: false,
        pay_in_advance: false,
    };
    let b = weekly.boundaries();
    assert_eq!(
        (b.from_date, b.to_date, b.charges_duration_days),
        (d(2024, 1, 1), d(2024, 1, 7), 7)
    );

    let quarterly = PeriodDates {
        interval: Interval::Quarterly,
        billing_at: d(2024, 4, 1),
        ..weekly
    };
    let q = quarterly.boundaries();
    assert_eq!(
        (q.from_date, q.to_date, q.charges_duration_days),
        (d(2024, 1, 1), d(2024, 3, 31), 91)
    );

    let semiannual = PeriodDates {
        interval: Interval::Semiannual,
        billing_at: d(2024, 7, 1),
        ..weekly
    };
    let s = semiannual.boundaries();
    assert_eq!(
        (s.from_date, s.to_date, s.charges_duration_days),
        (d(2024, 1, 1), d(2024, 6, 30), 182)
    );

    let yearly = PeriodDates {
        interval: Interval::Yearly,
        billing_at: d(2025, 1, 1),
        ..weekly
    };
    let y = yearly.boundaries();
    assert_eq!(
        (y.from_date, y.to_date, y.charges_duration_days),
        (d(2024, 1, 1), d(2024, 12, 31), 366)
    );
}

#[test]
fn weekly_anniversary_boundaries() {
    // subscription on a Wednesday; period is that Wed → the following Tue.
    let weekly = PeriodDates {
        interval: Interval::Weekly,
        subscription_at: d(2024, 1, 3), // Wednesday
        billing_at: d(2024, 1, 10),     // Wednesday
        anniversary: true,
        pay_in_advance: false,
    };
    let b = weekly.boundaries();
    assert_eq!(
        (b.from_date, b.to_date, b.charges_duration_days),
        (d(2024, 1, 3), d(2024, 1, 9), 7)
    );
}

// ---------------------------------------------------------------- termination

#[test]
fn terminate_mid_period_prorated_invoice() {
    // Plan $310.00 (31000c) monthly ⇒ single_day_price = 1000c/day.
    // Terminate on Jan 10 ⇒ 10 days billed ⇒ subscription fee 10000c.
    // Usage: standard @0.5, 100 units ⇒ 5000c. Total 15000c.
    let plan = plan_usd(31000);
    let sub = subscription("sub_1");
    let m = metric();
    let ch = charge_std("0.5");
    let evs: Vec<_> = [40, 35, 25]
        .iter()
        .map(|v| {
            let mut e = charge_to_fee_events(*v);
            e.external_subscription_id = Some("sub_1".into());
            e
        })
        .collect();

    let invoice = blank_invoice();
    let charges = [ChargeInput {
        filters: &[],
        persisted_events: &[],
        metric: &m,
        charge: &ch,
        events: &evs,
        period: None,
    }];
    let res = terminate_subscription(
        invoice,
        &plan,
        &sub,
        &jan_dates(),
        d(2024, 1, 10),
        &charges,
        &[],
    )
    .unwrap();

    assert_eq!(res.days_billed, 10);
    assert_eq!(res.fees[0].amount_cents, 10000); // prorated subscription fee
    assert_eq!(res.fees[1].amount_cents, 5000); // usage
    assert_eq!(res.invoice.total_amount_cents, 15000);
    assert_eq!(res.subscription.status, SubscriptionStatus::Terminated);
    assert!(res.subscription.terminated_at.is_some());
}

fn charge_to_fee_events(value: i64) -> rago_core::models::Event {
    rago_core::models::Event {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        customer_id: None,
        subscription_id: None,
        transaction_id: Uuid::new_v4().to_string(),
        code: "api".into(),
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

// ---------------------------------------------------------------- upgrade

#[test]
fn upgrade_with_proration_and_old_plan_credit() {
    // Old plan $310 (31000c ⇒ 1000c/day), new plan $620 (62000c ⇒ 2000c/day).
    // Upgrade on Jan 11 ⇒ remaining days = 21.
    //   old-plan credit = 21 × 1000 = 21000c (unused days refunded).
    //   new-plan bill  = 21 × 2000 = 42000c.
    let old_plan = plan_usd(31000);
    let new_plan = plan_usd(62000);
    let sub = subscription("sub_1");

    let invoice = blank_invoice();
    let res = upgrade_subscription(
        invoice,
        &old_plan,
        &new_plan,
        &sub,
        &jan_dates(),
        d(2024, 1, 11),
        &[],
        &[],
        &[],
    )
    .unwrap();

    assert_eq!(res.new_plan_days_billed, 21);
    assert_eq!(res.old_plan_credit_cents, 21000);
    assert_eq!(res.fees[0].amount_cents, 42000); // new plan prorated fee
    assert_eq!(res.invoice.total_amount_cents, 42000);
    assert_eq!(res.new_subscription.previous_subscription_id, Some(sub.id));
    assert_eq!(res.old_subscription.status, SubscriptionStatus::Terminated);
    assert_eq!(res.new_subscription.status, SubscriptionStatus::Active);
}

#[test]
fn date_diff_is_inclusive() {
    assert_eq!(date_diff(d(2024, 1, 1), d(2024, 1, 10)), 10);
    assert_eq!(date_diff(d(2024, 1, 11), d(2024, 1, 31)), 21);
    assert_eq!(date_diff(d(2024, 1, 1), d(2024, 1, 1)), 1);
}
