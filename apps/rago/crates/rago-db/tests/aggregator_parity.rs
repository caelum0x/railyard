//! Live-Postgres aggregator parity suite.
//!
//! Rago's charge-model math is locked to Lago by a pure-fixture parity suite.
//! This suite closes the remaining gap: it proves the DB/event-store-backed
//! **aggregators** — how usage events are aggregated over a billing period
//! *before* the charge model prices them — against Lago's authoritative
//! numbers, over a real event store rather than a hand-built `&[Event]`.
//!
//! Each test inserts the exact event sequence from a Lago aggregation spec into
//! Postgres, reads it back through the production read scope
//! (`Db::events_for_external_subscription`, i.e. `Events::Stores::PostgresStore#events`),
//! then runs Rago's aggregator (`rago_billing::aggregation`) over the fetched
//! events and asserts the aggregate equals Lago's asserted value (exact
//! `rust_decimal`). The Lago spec each case maps to is cited inline.
//!
//! Gated like the other live tests: `#[sqlx::test]` provisions an isolated,
//! migrated database per test, so DB-less CI simply skips them.
//!
//! Coverage & Lago spec mapping:
//!   * sum_agg          → spec/services/billable_metrics/aggregations/sum_service_spec.rb
//!   * count_agg        → …/count_service_spec.rb
//!   * max_agg          → …/max_service_spec.rb
//!   * latest_agg       → …/latest_service_spec.rb
//!   * unique_count_agg → …/unique_count_service_spec.rb (incl. add/remove operation_type)
//!   * weighted_sum_agg → …/weighted_sum_service_spec.rb
//!   * prorated sum     → spec/services/billable_metrics/prorated_aggregations/sum_service_spec.rb
//!   * dedup            → the unique index on (org, external_subscription, transaction_id)
//!     is exercised through every aggregator: a duplicate transaction_id must be
//!     metered once.
//!
//! ClickHouse note: Rago's ClickHouse store scope (`rago-clickhouse`) is byte-
//! for-byte the same period/boundary/order as the Postgres scope, so these
//! aggregate numbers are backend-independent; this suite proves the Postgres
//! path (no throwaway ClickHouse booted).

use chrono::{NaiveDate, NaiveDateTime, Utc};
use rago_billing::aggregation::{aggregate, prorated_sum, weighted_sum};
use rago_core::enums::AggregationType;
use rago_core::models::{BillableMetric, Event};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

const CODE: &str = "bm_code";
const SUB: &str = "sub_ext_1";

fn dt(y: i32, m: u32, d: u32, hh: u32, mm: u32, ss: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, m, d)
        .unwrap()
        .and_hms_opt(hh, mm, ss)
        .unwrap()
}

/// Lago's `to_datetime` = `end_of_day` = `23:59:59.999`. The sub-second matters
/// for weighted_sum, which ceils `to` to the whole second (Lago
/// `to_datetime.ceil`) so the last active event's span covers the full period.
fn end_of_day(y: i32, m: u32, d: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, m, d)
        .unwrap()
        .and_hms_milli_opt(23, 59, 59, 999)
        .unwrap()
}

fn metric(agg: AggregationType, field: Option<&str>) -> BillableMetric {
    let now = Utc::now();
    BillableMetric {
        id: Uuid::new_v4(),
        organization_id: Uuid::new_v4(),
        name: "m".into(),
        code: CODE.into(),
        description: None,
        aggregation_type: agg,
        field_name: field.map(|s| s.to_string()),
        recurring: false,
        expression: None,
        rounding_function: None,
        rounding_precision: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
    }
}

fn event(org: Uuid, txn: &str, ts: NaiveDateTime, props: Value) -> Event {
    let now = Utc::now();
    Event {
        id: Uuid::new_v4(),
        organization_id: org,
        customer_id: None,
        subscription_id: None,
        transaction_id: txn.to_string(),
        code: CODE.to_string(),
        properties: props,
        external_customer_id: None,
        external_subscription_id: Some(SUB.to_string()),
        timestamp: Some(ts),
        precise_total_amount_cents: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        persisted: true,
    }
}

/// Read scope the aggregators consume (`Events::Stores::PostgresStore#events`).
async fn fetch(db: &rago_db::Db, org: Uuid, from: NaiveDateTime, to: NaiveDateTime) -> Vec<Event> {
    db.events_for_external_subscription(org, SUB, CODE, from, to)
        .await
        .unwrap()
}

/// Events with a numeric `field` property, mapped to `(timestamp, value)`.
fn as_ts_values(events: &[Event], field: &str) -> Vec<(NaiveDateTime, Decimal)> {
    events
        .iter()
        .map(|e| (e.timestamp.unwrap(), e.field_value(field)))
        .collect()
}

/// Events matching a property equality — the harness stand-in for Lago's
/// charge/matching filters, which scope the event-store read.
fn matching(events: &[Event], key: &str, value: &str) -> Vec<Event> {
    events
        .iter()
        .filter(|e| e.properties.get(key).and_then(|v| v.as_str()) == Some(value))
        .cloned()
        .collect()
}

// ---------------------------------------------------------------------------
// sum_agg — sum_service_spec.rb
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn sum_agg_matches_lago(pool: PgPool) {
    // sum_service_spec.rb "aggregates the events": 4 in-period events of 12,
    // 2 out-of-period events of 2.5. Non-recurring ⇒ from-boundary applied.
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);

    // out-of-period (before `from`)
    for i in 0..2 {
        db.insert_event(&event(
            org,
            &format!("old{i}"),
            dt(2023, 3, 1, 0, 0, 0),
            json!({"total_count": 2.5}),
        ))
        .await
        .unwrap();
    }
    // in-period
    for i in 0..4 {
        db.insert_event(&event(
            org,
            &format!("new{i}"),
            dt(2023, 5, 30, 0, 0, 0),
            json!({"total_count": 12}),
        ))
        .await
        .unwrap();
    }

    let events = fetch(&db, org, from, to).await;
    assert_eq!(
        events.len(),
        4,
        "from-boundary excludes the 2 out-of-period events"
    );

    // options {free_units_per_events: 2, free_units_per_total_aggregation: 30}
    let r = aggregate(
        &metric(AggregationType::SumAgg, Some("total_count")),
        &events,
        2,
        dec!(30),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(48)); // Lago: 48
    assert_eq!(r.count, 4);
    assert_eq!(r.running_total, vec![dec!(12), dec!(24)]); // Lago: [12, 24]
}

#[sqlx::test]
async fn sum_agg_recurring_uses_full_history(pool: PgPool) {
    // sum_service_spec.rb "when billable metric is recurring": 53, count 6,
    // running_total [2.5, 5]. Recurring ⇒ no from-boundary (read from history).
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let recurring_from = dt(2000, 1, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);

    for i in 0..2 {
        db.insert_event(&event(
            org,
            &format!("old{i}"),
            dt(2023, 3, 1, 0, 0, 0),
            json!({"total_count": 2.5}),
        ))
        .await
        .unwrap();
    }
    for i in 0..4 {
        db.insert_event(&event(
            org,
            &format!("new{i}"),
            dt(2023, 5, 30, 0, 0, 0),
            json!({"total_count": 12}),
        ))
        .await
        .unwrap();
    }

    let events = fetch(&db, org, recurring_from, to).await;
    let r = aggregate(
        &metric(AggregationType::SumAgg, Some("total_count")),
        &events,
        2,
        dec!(30),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(53)); // Lago: 53
    assert_eq!(r.count, 6);
    assert_eq!(r.running_total, vec![dec!(2.5), dec!(5)]); // Lago: [2.5, 5]
}

#[sqlx::test]
async fn sum_agg_out_of_bounds_is_zero(pool: PgPool) {
    // sum_service_spec.rb "when events are out of bounds": events at to+1day.
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);
    for i in 0..4 {
        db.insert_event(&event(
            org,
            &format!("oob{i}"),
            dt(2023, 6, 1, 0, 0, 0),
            json!({"total_count": 12}),
        ))
        .await
        .unwrap();
    }
    let events = fetch(&db, org, from, to).await;
    let r = aggregate(
        &metric(AggregationType::SumAgg, Some("total_count")),
        &events,
        0,
        dec!(0),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(0));
    assert_eq!(r.count, 0);
}

#[sqlx::test]
async fn sum_agg_float_and_filter_and_rounding(pool: PgPool) {
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);

    // "when properties is a float": 4×12 + 4.5 = 52.5
    for i in 0..4 {
        db.insert_event(&event(
            org,
            &format!("f{i}"),
            dt(2023, 5, 20, 0, 0, 0),
            json!({"total_count": 12}),
        ))
        .await
        .unwrap();
    }
    db.insert_event(&event(
        org,
        "ff",
        dt(2023, 5, 21, 0, 0, 0),
        json!({"total_count": 4.5}),
    ))
    .await
    .unwrap();
    let events = fetch(&db, org, from, to).await;
    let r = aggregate(
        &metric(AggregationType::SumAgg, Some("total_count")),
        &events,
        0,
        dec!(0),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(52.5)); // Lago: 52.5

    // "when filters are given" region=europe: (12 + 8) = 20, count 2
    let org2 = Uuid::new_v4();
    db.insert_event(&event(
        org2,
        "e1",
        dt(2023, 5, 20, 0, 0, 0),
        json!({"total_count": 12, "region": "europe"}),
    ))
    .await
    .unwrap();
    db.insert_event(&event(
        org2,
        "e2",
        dt(2023, 5, 20, 0, 0, 0),
        json!({"total_count": 8, "region": "europe"}),
    ))
    .await
    .unwrap();
    db.insert_event(&event(
        org2,
        "a1",
        dt(2023, 5, 20, 0, 0, 0),
        json!({"total_count": 12, "region": "africa"}),
    ))
    .await
    .unwrap();
    let all = fetch(&db, org2, from, to).await;
    let eu = matching(&all, "region", "europe");
    let r = aggregate(
        &metric(AggregationType::SumAgg, Some("total_count")),
        &eu,
        2,
        dec!(30),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(20)); // Lago: 20
    assert_eq!(r.count, 2);
    assert_eq!(r.running_total, vec![dec!(12), dec!(20)]); // Lago: [12, 20]

    // "when rounding is configured" ceil precision 2: 3×12 + 12.434 → 48.44
    let org3 = Uuid::new_v4();
    for i in 0..3 {
        db.insert_event(&event(
            org3,
            &format!("r{i}"),
            dt(2023, 5, 20, 0, 0, 0),
            json!({"total_count": 12}),
        ))
        .await
        .unwrap();
    }
    db.insert_event(&event(
        org3,
        "rl",
        dt(2023, 5, 21, 0, 0, 0),
        json!({"total_count": 12.434}),
    ))
    .await
    .unwrap();
    let events = fetch(&db, org3, from, to).await;
    let mut m = metric(AggregationType::SumAgg, Some("total_count"));
    m.rounding_function = Some("ceil".into());
    m.rounding_precision = Some(2);
    let r = aggregate(&m, &events, 0, dec!(0)).unwrap();
    assert_eq!(r.aggregation, dec!(48.44)); // Lago: 48.44
}

#[sqlx::test]
async fn sum_agg_grouped_by_property(pool: PgPool) {
    // sum_service_spec.rb ".grouped_by aggregation": one 12-event per agent_name.
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);
    for agent in ["aragorn", "frodo", "gimli", "legolas"] {
        db.insert_event(&event(
            org,
            agent,
            dt(2023, 5, 20, 0, 0, 0),
            json!({"total_count": 12, "agent_name": agent}),
        ))
        .await
        .unwrap();
    }
    let all = fetch(&db, org, from, to).await;
    for agent in ["aragorn", "frodo", "gimli", "legolas"] {
        let g = matching(&all, "agent_name", agent);
        let r = aggregate(
            &metric(AggregationType::SumAgg, Some("total_count")),
            &g,
            0,
            dec!(0),
        )
        .unwrap();
        assert_eq!(r.aggregation, dec!(12), "group {agent}"); // Lago: 12 per group
        assert_eq!(r.count, 1);
    }
}

#[sqlx::test]
async fn sum_agg_dedups_duplicate_transaction(pool: PgPool) {
    // The unique index makes a re-sent transaction_id a no-op; the aggregator
    // must meter it once (would report 60 instead of 48 on a double-count).
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);
    for i in 0..4 {
        assert!(db
            .insert_event(&event(
                org,
                &format!("t{i}"),
                dt(2023, 5, 20, 0, 0, 0),
                json!({"total_count": 12})
            ))
            .await
            .unwrap());
    }
    // Re-send transaction t0 (idempotent no-op).
    assert!(!db
        .insert_event(&event(
            org,
            "t0",
            dt(2023, 5, 20, 0, 0, 0),
            json!({"total_count": 12})
        ))
        .await
        .unwrap());

    let events = fetch(&db, org, from, to).await;
    assert_eq!(events.len(), 4, "duplicate transaction_id counted once");
    let r = aggregate(
        &metric(AggregationType::SumAgg, Some("total_count")),
        &events,
        0,
        dec!(0),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(48)); // Lago: 48, not 60
    assert_eq!(r.count, 4);
}

// ---------------------------------------------------------------------------
// count_agg — count_service_spec.rb
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn count_agg_matches_lago(pool: PgPool) {
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);
    for i in 0..4 {
        db.insert_event(&event(
            org,
            &format!("c{i}"),
            dt(2023, 5, 20, 0, 0, 0),
            json!({}),
        ))
        .await
        .unwrap();
    }
    let events = fetch(&db, org, from, to).await;
    let r = aggregate(
        &metric(AggregationType::CountAgg, None),
        &events,
        0,
        dec!(0),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(4)); // Lago: 4

    // "when filters are given" cloud=AWS AND region=europe: 2
    let org2 = Uuid::new_v4();
    db.insert_event(&event(
        org2,
        "x1",
        dt(2023, 5, 20, 0, 0, 0),
        json!({"cloud":"AWS","region":"europe"}),
    ))
    .await
    .unwrap();
    db.insert_event(&event(
        org2,
        "x2",
        dt(2023, 5, 20, 0, 0, 0),
        json!({"cloud":"AWS","region":"europe"}),
    ))
    .await
    .unwrap();
    db.insert_event(&event(
        org2,
        "x3",
        dt(2023, 5, 20, 0, 0, 0),
        json!({"cloud":"AWS","region":"africa"}),
    ))
    .await
    .unwrap();
    let all = fetch(&db, org2, from, to).await;
    let matched: Vec<Event> = all
        .iter()
        .filter(|e| {
            e.properties.get("cloud").and_then(|v| v.as_str()) == Some("AWS")
                && e.properties.get("region").and_then(|v| v.as_str()) == Some("europe")
        })
        .cloned()
        .collect();
    let r = aggregate(
        &metric(AggregationType::CountAgg, None),
        &matched,
        0,
        dec!(0),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(2)); // Lago: 2
}

#[sqlx::test]
async fn count_agg_dedups_duplicate_transaction(pool: PgPool) {
    // count_service_spec.rb ClickHouse "with deduplication": 4 events sharing a
    // transaction_id count as 1. Postgres enforces this via the unique index.
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);
    for _ in 0..4 {
        db.insert_event(&event(org, "123456", dt(2023, 5, 20, 0, 0, 0), json!({})))
            .await
            .unwrap();
    }
    let events = fetch(&db, org, from, to).await;
    let r = aggregate(
        &metric(AggregationType::CountAgg, None),
        &events,
        0,
        dec!(0),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(1)); // Lago (dedup): 1
}

// ---------------------------------------------------------------------------
// max_agg — max_service_spec.rb
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn max_agg_matches_lago(pool: PgPool) {
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);
    for (i, v) in [3, 5, 2, 7].iter().enumerate() {
        db.insert_event(&event(
            org,
            &format!("m{i}"),
            dt(2023, 5, 10, 0, 0, 0),
            json!({"total_count": v}),
        ))
        .await
        .unwrap();
    }
    db.insert_event(&event(
        org,
        "mmax",
        dt(2023, 5, 11, 0, 0, 0),
        json!({"total_count": 12}),
    ))
    .await
    .unwrap();
    let events = fetch(&db, org, from, to).await;
    let r = aggregate(
        &metric(AggregationType::MaxAgg, Some("total_count")),
        &events,
        0,
        dec!(0),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(12)); // Lago: 12
    assert_eq!(r.count, 5);

    // "when filters are given" region=europe: max over {8} = 8, count 1
    let org2 = Uuid::new_v4();
    db.insert_event(&event(
        org2,
        "e",
        dt(2023, 5, 10, 0, 0, 0),
        json!({"total_count": 8, "region":"europe"}),
    ))
    .await
    .unwrap();
    db.insert_event(&event(
        org2,
        "a",
        dt(2023, 5, 10, 0, 0, 0),
        json!({"total_count": 12, "region":"africa"}),
    ))
    .await
    .unwrap();
    let all = fetch(&db, org2, from, to).await;
    let eu = matching(&all, "region", "europe");
    let r = aggregate(
        &metric(AggregationType::MaxAgg, Some("total_count")),
        &eu,
        0,
        dec!(0),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(8)); // Lago: 8
    assert_eq!(r.count, 1);
}

// ---------------------------------------------------------------------------
// latest_agg — latest_service_spec.rb
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn latest_agg_matches_lago(pool: PgPool) {
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);
    for i in 0..4 {
        db.insert_event(&event(
            org,
            &format!("l{i}"),
            dt(2023, 5, 10, 0, 0, 0),
            json!({"total_count": 18}),
        ))
        .await
        .unwrap();
    }
    db.insert_event(&event(
        org,
        "llast",
        dt(2023, 5, 11, 0, 0, 0),
        json!({"total_count": 14}),
    ))
    .await
    .unwrap();
    let events = fetch(&db, org, from, to).await;
    let r = aggregate(
        &metric(AggregationType::LatestAgg, Some("total_count")),
        &events,
        0,
        dec!(0),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(14)); // Lago: 14 (last by timestamp)
    assert_eq!(r.count, 5);

    // "when properties is negative": latest clamps to 0
    let org2 = Uuid::new_v4();
    db.insert_event(&event(
        org2,
        "neg",
        dt(2023, 5, 10, 0, 0, 0),
        json!({"total_count": -5}),
    ))
    .await
    .unwrap();
    let events = fetch(&db, org2, from, to).await;
    let r = aggregate(
        &metric(AggregationType::LatestAgg, Some("total_count")),
        &events,
        0,
        dec!(0),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(0)); // Lago: 0
}

// ---------------------------------------------------------------------------
// unique_count_agg — unique_count_service_spec.rb
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn unique_count_recurring_vs_non_recurring(pool: PgPool) {
    // unique_count_service_spec.rb: a persisted event before the period plus a
    // new event in the period. Recurring ⇒ 2 (history counted); non-recurring
    // ⇒ 1 (only the in-period event).
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let period_from = dt(2022, 7, 9, 0, 0, 0);
    let to = dt(2022, 8, 8, 23, 59, 59);
    let recurring_from = dt(2000, 1, 1, 0, 0, 0);

    db.insert_event(&event(
        org,
        "persisted",
        dt(2022, 6, 9, 0, 0, 0),
        json!({"unique_id": "aaa"}),
    ))
    .await
    .unwrap();
    db.insert_event(&event(
        org,
        "inperiod",
        dt(2022, 7, 19, 0, 0, 0),
        json!({"unique_id": "bbb"}),
    ))
    .await
    .unwrap();

    let recurring = fetch(&db, org, recurring_from, to).await;
    let r = aggregate(
        &metric(AggregationType::UniqueCountAgg, Some("unique_id")),
        &recurring,
        0,
        dec!(0),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(2)); // Lago recurring: 2

    let non_recurring = fetch(&db, org, period_from, to).await;
    let r = aggregate(
        &metric(AggregationType::UniqueCountAgg, Some("unique_id")),
        &non_recurring,
        0,
        dec!(0),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(1)); // Lago non-recurring: 1
}

#[sqlx::test]
async fn unique_count_add_then_remove_nets_zero(pool: PgPool) {
    // unique_count_service_spec.rb "with persisted metrics terminated in the
    // period": an add followed by a `remove` for the same unique_id → 0.
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let from = dt(2000, 1, 1, 0, 0, 0);
    let to = dt(2022, 8, 8, 23, 59, 59);

    db.insert_event(&event(
        org,
        "add",
        dt(2022, 6, 9, 0, 0, 0),
        json!({"unique_id": "aaa"}),
    ))
    .await
    .unwrap();
    db.insert_event(&event(
        org,
        "rem",
        dt(2022, 7, 24, 0, 0, 0),
        json!({"unique_id": "aaa", "operation_type": "remove"}),
    ))
    .await
    .unwrap();

    let events = fetch(&db, org, from, to).await;
    let r = aggregate(
        &metric(AggregationType::UniqueCountAgg, Some("unique_id")),
        &events,
        0,
        dec!(0),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(0)); // Lago: 0 (remove cancels the add)
}

#[sqlx::test]
async fn unique_count_grouped_by_property(pool: PgPool) {
    // unique_count_service_spec.rb ".grouped_by_aggregation": one persisted +
    // one in-period unique per agent → 2 per agent (recurring).
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let from = dt(2000, 1, 1, 0, 0, 0);
    let to = dt(2022, 8, 8, 23, 59, 59);
    for agent in ["aragorn", "frodo"] {
        db.insert_event(&event(
            org,
            &format!("p_{agent}"),
            dt(2022, 6, 9, 0, 0, 0),
            json!({"unique_id": format!("{agent}-1"), "agent_name": agent}),
        ))
        .await
        .unwrap();
        db.insert_event(&event(
            org,
            &format!("n_{agent}"),
            dt(2022, 7, 19, 0, 0, 0),
            json!({"unique_id": format!("{agent}-2"), "agent_name": agent}),
        ))
        .await
        .unwrap();
    }
    let all = fetch(&db, org, from, to).await;
    for agent in ["aragorn", "frodo"] {
        let g = matching(&all, "agent_name", agent);
        let r = aggregate(
            &metric(AggregationType::UniqueCountAgg, Some("unique_id")),
            &g,
            0,
            dec!(0),
        )
        .unwrap();
        assert_eq!(r.aggregation, dec!(2), "group {agent}"); // Lago: 2 per group
    }
}

// ---------------------------------------------------------------------------
// weighted_sum_agg — weighted_sum_service_spec.rb
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn weighted_sum_time_weighting(pool: PgPool) {
    // weighted_sum_service_spec.rb "aggregates the events": the canonical 7-event
    // timeline over August 2023 → 0.02218, count 7.
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let from = dt(2023, 8, 1, 0, 0, 0);
    let to = end_of_day(2023, 8, 31); // Lago to_datetime (…​.999); weighted_sum ceils it
    let seq = [
        (dt(2023, 8, 1, 0, 0, 0), 2),
        (dt(2023, 8, 1, 1, 0, 0), 3),
        (dt(2023, 8, 1, 1, 30, 0), 1),
        (dt(2023, 8, 1, 2, 0, 0), -4),
        (dt(2023, 8, 1, 4, 0, 0), -2),
        (dt(2023, 8, 1, 5, 0, 0), 10),
        (dt(2023, 8, 1, 5, 30, 0), -10),
    ];
    for (i, (ts, v)) in seq.iter().enumerate() {
        db.insert_event(&event(org, &format!("w{i}"), *ts, json!({"value": v})))
            .await
            .unwrap();
    }
    let events = fetch(&db, org, from, to).await;
    let vals = as_ts_values(&events, "value");
    let r = weighted_sum(&vals, from, to, 31, dec!(0));
    assert_eq!(r.aggregation.round_dp(5), dec!(0.02218)); // Lago: 0.02218
    assert_eq!(r.count, 7);
}

#[sqlx::test]
async fn weighted_sum_single_event_full_period(pool: PgPool) {
    // weighted_sum_service_spec.rb "with a single event": value 1000 → 1000.0.
    // Regression guard for the boundary bug: Rago previously truncated the final
    // span with num_seconds() and used the raw (…​.999) `to`, yielding
    // 999.99963. It now ceils `to` to the whole second (Lago `to_datetime.ceil`)
    // and measures spans at sub-second precision.
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let from = dt(2023, 8, 1, 0, 0, 0);
    let to = end_of_day(2023, 8, 31);
    db.insert_event(&event(
        org,
        "solo",
        dt(2023, 8, 1, 0, 0, 0),
        json!({"value": 1000}),
    ))
    .await
    .unwrap();
    let events = fetch(&db, org, from, to).await;
    let vals = as_ts_values(&events, "value");
    let r = weighted_sum(&vals, from, to, 31, dec!(0));
    assert_eq!(r.aggregation.round_dp(5), dec!(1000.0)); // Lago: 1000.0
    assert_eq!(r.count, 1);
}

#[sqlx::test]
async fn weighted_sum_same_timestamp(pool: PgPool) {
    // weighted_sum_service_spec.rb "with events with the same timestamp":
    // two value-3 events at the same instant → exactly 6.
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let from = dt(2023, 8, 1, 0, 0, 0);
    let to = end_of_day(2023, 8, 31);
    db.insert_event(&event(
        org,
        "s1",
        dt(2023, 8, 1, 0, 0, 0),
        json!({"value": 3}),
    ))
    .await
    .unwrap();
    db.insert_event(&event(
        org,
        "s2",
        dt(2023, 8, 1, 0, 0, 0),
        json!({"value": 3}),
    ))
    .await
    .unwrap();
    let events = fetch(&db, org, from, to).await;
    let vals = as_ts_values(&events, "value");
    let r = weighted_sum(&vals, from, to, 31, dec!(0));
    assert_eq!(r.aggregation.round_dp(5), dec!(6)); // Lago: 6
    assert_eq!(r.count, 2);
}

#[sqlx::test]
async fn weighted_sum_recurring_initial_value(pool: PgPool) {
    // weighted_sum_service_spec.rb "when billable metric is recurring" / no
    // events: the persisted recurring value (1000) is the aggregation.
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let from = dt(2023, 8, 1, 0, 0, 0);
    let to = end_of_day(2023, 8, 31);
    let events = fetch(&db, org, from, to).await; // none
    let vals = as_ts_values(&events, "value");
    let r = weighted_sum(&vals, from, to, 31, dec!(1000));
    assert_eq!(r.aggregation.round_dp(5), dec!(1000.0)); // Lago: 1000.0
    assert_eq!(r.count, 0);
    assert_eq!(r.variation, Some(dec!(0))); // Lago: variation 0
}

#[sqlx::test]
async fn weighted_sum_grouped_by_property(pool: PgPool) {
    // weighted_sum_service_spec.rb ".grouped_by aggregation": the 7-event
    // timeline duplicated per agent → 0.02218 per agent.
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let from = dt(2023, 8, 1, 0, 0, 0);
    let to = end_of_day(2023, 8, 31);
    let seq = [
        (dt(2023, 8, 1, 0, 0, 0), 2),
        (dt(2023, 8, 1, 1, 0, 0), 3),
        (dt(2023, 8, 1, 1, 30, 0), 1),
        (dt(2023, 8, 1, 2, 0, 0), -4),
        (dt(2023, 8, 1, 4, 0, 0), -2),
        (dt(2023, 8, 1, 5, 0, 0), 10),
        (dt(2023, 8, 1, 5, 30, 0), -10),
    ];
    for agent in ["aragorn", "frodo"] {
        for (i, (ts, v)) in seq.iter().enumerate() {
            db.insert_event(&event(
                org,
                &format!("{agent}{i}"),
                *ts,
                json!({"value": v, "agent_name": agent}),
            ))
            .await
            .unwrap();
        }
    }
    let all = fetch(&db, org, from, to).await;
    for agent in ["aragorn", "frodo"] {
        let g = matching(&all, "agent_name", agent);
        let vals = as_ts_values(&g, "value");
        let r = weighted_sum(&vals, from, to, 31, dec!(0));
        assert_eq!(r.aggregation.round_dp(5), dec!(0.02218), "group {agent}"); // Lago: 0.02218 per group
        assert_eq!(r.count, 7);
    }
}

// ---------------------------------------------------------------------------
// prorated sum — prorated_aggregations/sum_service_spec.rb
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn prorated_sum_within_period(pool: PgPool) {
    // prorated_aggregations/sum_service_spec.rb: two value-12 events at
    // from+25days (2023-05-26), period 31 days, to 2023-05-31 ⇒ 12*6/31 twice.
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);
    for i in 0..2 {
        db.insert_event(&event(
            org,
            &format!("p{i}"),
            dt(2023, 5, 26, 0, 0, 0),
            json!({"total_count": 12}),
        ))
        .await
        .unwrap();
    }
    let events = fetch(&db, org, from, to).await;
    let pairs: Vec<(NaiveDate, Decimal)> = events
        .iter()
        .map(|e| (e.timestamp.unwrap().date(), e.field_value("total_count")))
        .collect();
    let r = prorated_sum(&pairs, to.date(), 31);
    assert_eq!(r.aggregation, dec!(4.64517)); // Lago: 2 × 12*6/31, ceil5 = 4.64517
    assert_eq!(r.count, 2);
}

#[sqlx::test]
async fn prorated_sum_boundary_last_day_and_filter(pool: PgPool) {
    // prorated_aggregations/sum_service_spec.rb "when filters are given":
    // two europe events (12, 8) on the last day → (12 + 8)/31, ceil5 = 0.64517.
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);
    db.insert_event(&event(
        org,
        "e1",
        dt(2023, 5, 31, 0, 0, 0),
        json!({"total_count": 12, "region": "europe"}),
    ))
    .await
    .unwrap();
    db.insert_event(&event(
        org,
        "e2",
        dt(2023, 5, 31, 0, 0, 0),
        json!({"total_count": 8, "region": "europe"}),
    ))
    .await
    .unwrap();
    db.insert_event(&event(
        org,
        "a1",
        dt(2023, 5, 31, 0, 0, 0),
        json!({"total_count": 12, "region": "africa"}),
    ))
    .await
    .unwrap();
    let all = fetch(&db, org, from, to).await;
    let eu = matching(&all, "region", "europe");
    let pairs: Vec<(NaiveDate, Decimal)> = eu
        .iter()
        .map(|e| (e.timestamp.unwrap().date(), e.field_value("total_count")))
        .collect();
    let r = prorated_sum(&pairs, to.date(), 31);
    assert_eq!(r.aggregation, dec!(0.64517)); // Lago: 0.64517
    assert_eq!(r.count, 2);
}
