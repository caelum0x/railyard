//! Confirms **pay-in-arrears is the default billing path** and works alongside
//! pay-in-advance.
//!
//! Lago bills subscriptions in arrears by default (`plan.pay_in_advance = false`):
//! at a billing boundary it bills the period that just *completed*. A
//! pay-in-advance plan instead bills the flat fee for the *upcoming* period while
//! still metering usage (charges) for the completed one. `Subscriptions::Dates::
//! MonthlyService` encodes both; this test pins the boundaries and the fees for
//! each, over the same billing date.

use chrono::{NaiveDate, Utc};
use rago_billing::dates::MonthlyDates;
use rago_billing::subscription_fee::{build_subscription_fee, Proration};
use rago_core::enums::{
    AggregationType, BillingTime, ChargeModel, PlanInterval, SubscriptionStatus,
};
use rago_core::models::{BillableMetric, Charge, Event, Plan, Subscription};
use rago_core::money::Currency;
use rust_decimal_macros::dec;
use serde_json::json;
use uuid::Uuid;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

fn plan(pay_in_advance: bool) -> Plan {
    let now = Utc::now();
    Plan {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        name: "p".into(),
        code: "p".into(),
        interval: Some(PlanInterval::Monthly),
        description: None,
        amount_cents: Some(10_000),
        amount_currency: "USD".into(),
        trial_period: None,
        pay_in_advance,
        bill_charges_monthly: None,
        parent_id: None,
        invoice_display_name: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn subscription() -> Subscription {
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

fn dates(pay_in_advance: bool) -> MonthlyDates {
    MonthlyDates {
        subscription_at: d(2023, 1, 1),
        billing_at: d(2023, 6, 1),
        anniversary: false,
        pay_in_advance,
        current_usage: false,
    }
}

#[test]
fn in_arrears_is_default_and_bills_completed_period() {
    // Default (pay-in-arrears): billing on 2023-06-01 bills completed May.
    let dates = dates(false);
    let b = dates.boundaries();
    assert_eq!(b.from_date, d(2023, 5, 1));
    assert_eq!(b.to_date, d(2023, 5, 31));
    assert_eq!(b.charges_from_date, d(2023, 5, 1));
    assert_eq!(b.charges_to_date, d(2023, 5, 31));

    // Full-period flat fee bills the whole plan amount.
    let plan = plan(false);
    let sub = subscription();
    let fee = build_subscription_fee(
        &plan,
        &sub,
        None,
        sub.organization_id,
        &dates,
        Proration::Full,
    );
    assert_eq!(fee.amount_cents, 10_000);
}

#[test]
fn pay_in_advance_bills_upcoming_period_but_meters_completed_usage() {
    // Pay-in-advance: flat fee for the upcoming June period, usage for May.
    let dates = dates(true);
    let b = dates.boundaries();
    assert_eq!(
        b.from_date,
        d(2023, 6, 1),
        "flat fee period is the upcoming month"
    );
    assert_eq!(b.to_date, d(2023, 6, 30));
    assert_eq!(
        b.charges_from_date,
        d(2023, 5, 1),
        "charges still meter the completed month"
    );
    assert_eq!(b.charges_to_date, d(2023, 5, 31));

    let plan = plan(true);
    let sub = subscription();
    let fee = build_subscription_fee(
        &plan,
        &sub,
        None,
        sub.organization_id,
        &dates,
        Proration::Full,
    );
    assert_eq!(fee.amount_cents, 10_000);
}

#[test]
fn in_arrears_usage_charge_bills_period_events() {
    // The default in-arrears charge path: aggregate May usage → charge fee.
    let now = Utc::now();
    let metric = BillableMetric {
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
        created_at: now,
        updated_at: now,
        deleted_at: None,
    };
    let charge = Charge {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        billable_metric_id: Some(metric.id),
        plan_id: None,
        code: "charge".into(),
        charge_model: ChargeModel::Standard,
        properties: json!({"amount": "0.10"}),
        pay_in_advance: false, // in-arrears charge (the default)
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
    };
    let ev = |v: i64| Event {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        customer_id: None,
        subscription_id: None,
        transaction_id: Uuid::new_v4().to_string(),
        code: "api".into(),
        properties: json!({ "value": v }),
        external_customer_id: None,
        external_subscription_id: None,
        timestamp: Some(d(2023, 5, 15).and_hms_opt(0, 0, 0).unwrap()),
        precise_total_amount_cents: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        persisted: true,
    };
    let events = vec![ev(100), ev(50), ev(30)]; // sum 180 units @ 0.10 = 18.00
    let sub = subscription();
    let fee =
        rago_billing::charge_to_fee(&metric, &charge, &sub, &events, Currency::new("USD"), None)
            .unwrap();
    assert_eq!(fee.units, dec!(180));
    assert_eq!(fee.amount_cents, 1_800); // $18.00
}
