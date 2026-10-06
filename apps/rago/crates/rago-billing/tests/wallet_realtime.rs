//! Real-time wallet debit: each event is priced at its marginal cost with the
//! regular charge models, so the Mint holds placed per event add up to exactly
//! what the period's invoice charges, whichever model the charge uses.

use chrono::Utc;
use rago_billing::wallet_realtime::{incremental_event_cost, plan_settlement, SettlementStep};
use rago_core::enums::{AggregationType, ChargeModel, SubscriptionStatus};
use rago_core::models::{BillableMetric, Charge, Event, Subscription};
use rago_core::money::Currency;
use serde_json::{json, Value};
use uuid::Uuid;

fn metric(agg: AggregationType) -> BillableMetric {
    let now = Utc::now();
    BillableMetric {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        name: "API calls".into(),
        code: "api_calls".into(),
        description: None,
        aggregation_type: agg,
        field_name: Some("value".into()),
        recurring: false,
        expression: None,
        rounding_function: None,
        rounding_precision: None,
        custom_aggregator: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn charge(metric_id: Uuid, model: ChargeModel, properties: Value) -> Charge {
    let now = Utc::now();
    Charge {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        billable_metric_id: Some(metric_id),
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

fn subscription() -> Subscription {
    let now = Utc::now();
    let org = Uuid::new_v4();
    Subscription {
        id: Uuid::new_v4(),
        organization_id: org,
        customer_id: Uuid::new_v4(),
        plan_id: Uuid::new_v4(),
        billing_entity_id: Some(org),
        external_id: "sub_rt".into(),
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
        created_at: now,
        updated_at: now,
    }
}

fn event(value: i64) -> Event {
    let now = Utc::now();
    Event {
        id: Uuid::new_v4(),
        organization_id: Uuid::nil(),
        customer_id: None,
        subscription_id: None,
        transaction_id: Uuid::new_v4().to_string(),
        code: "api_calls".into(),
        properties: json!({ "value": value }),
        external_customer_id: None,
        external_subscription_id: Some("sub_rt".into()),
        timestamp: None,
        precise_total_amount_cents: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        persisted: true,
    }
}

/// Holds placed event by event must sum to the whole-period fee.
fn assert_holds_sum_to_period_fee(m: &BillableMetric, c: &Charge, values: &[i64]) {
    let sub = subscription();
    let usd = Currency::new("USD");
    let mut seen: Vec<Event> = Vec::new();
    let mut held = 0i64;
    for v in values {
        let e = event(*v);
        held += incremental_event_cost(m, std::slice::from_ref(c), &[], &sub, &seen, &e, usd).unwrap();
        seen.push(e);
    }
    let period = rago_billing::charge_to_fee(m, c, &sub, &seen, usd, None).unwrap();
    assert_eq!(held, period.amount_cents, "{:?}", c.charge_model);
}

#[test]
fn standard_model_holds_the_unit_price_per_event() {
    let m = metric(AggregationType::SumAgg);
    let c = charge(m.id, ChargeModel::Standard, json!({ "amount": "0.25" }));
    let sub = subscription();
    let cost = incremental_event_cost(&m, std::slice::from_ref(&c), &[], &sub, &[], &event(4), Currency::new("USD")).unwrap();
    assert_eq!(cost, 100);
    assert_holds_sum_to_period_fee(&m, &c, &[4, 1, 7, 3]);
}

#[test]
fn graduated_model_prices_each_event_at_its_marginal_tier() {
    let m = metric(AggregationType::SumAgg);
    let c = charge(
        m.id,
        ChargeModel::Graduated,
        json!({ "graduated_ranges": [
            { "from_value": 0, "to_value": 10, "per_unit_amount": "1", "flat_amount": "0" },
            { "from_value": 11, "to_value": null, "per_unit_amount": "0.5", "flat_amount": "0" }
        ]}),
    );
    assert_holds_sum_to_period_fee(&m, &c, &[6, 6, 6, 2]);
}

#[test]
fn package_model_only_holds_when_a_new_package_starts() {
    let m = metric(AggregationType::SumAgg);
    let c = charge(
        m.id,
        ChargeModel::Package,
        json!({ "amount": "10", "package_size": 100, "free_units": 0 }),
    );
    let sub = subscription();
    let usd = Currency::new("USD");
    let first = event(50);
    assert_eq!(incremental_event_cost(&m, std::slice::from_ref(&c), &[], &sub, &[], &first, usd).unwrap(), 1_000);
    // Still inside the first package: nothing more to hold.
    assert_eq!(incremental_event_cost(&m, std::slice::from_ref(&c), &[], &sub, std::slice::from_ref(&first), &event(30), usd).unwrap(), 0);
    assert_holds_sum_to_period_fee(&m, &c, &[50, 30, 40, 90]);
}

#[test]
fn charges_on_other_metrics_are_ignored() {
    let m = metric(AggregationType::SumAgg);
    let other = charge(Uuid::new_v4(), ChargeModel::Standard, json!({ "amount": "5" }));
    let cost = incremental_event_cost(&m, &[other], &[], &subscription(), &[], &event(3), Currency::new("USD")).unwrap();
    assert_eq!(cost, 0);
}

#[test]
fn settlement_plan_never_captures_more_than_due() {
    let holds = vec![("a".to_string(), 1_000_000), ("b".to_string(), 2_000_000)];
    let steps = plan_settlement(&holds, 2_500_000);
    let captured: i64 = steps
        .iter()
        .map(|s| match s {
            SettlementStep::Capture { micros, .. } => *micros,
            SettlementStep::Release { .. } => 0,
        })
        .sum();
    assert_eq!(captured, 2_500_000);
}
