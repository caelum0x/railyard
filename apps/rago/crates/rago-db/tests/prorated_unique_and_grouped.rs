// Postgres-backed suite: opt in with `--features pg-tests` and DATABASE_URL set
// (see README "Running the tests"). A plain `cargo test` skips this file.
#![cfg(feature = "pg-tests")]

//! Live-Postgres parity for the two newly-closed aggregation gaps:
//!
//!   * **prorated_unique_count** — `BillableMetrics::ProratedAggregations::
//!     UniqueCountService` + `Events::Stores::Postgres::UniqueCountQuery#
//!     prorated_query`. Each add contributes the fraction of the period it stayed
//!     active (per-day add/remove dedup). Numbers are lifted from
//!     `spec/services/billable_metrics/prorated_aggregations/unique_count_service_spec.rb`
//!     (period 2022-07-09 .. 2022-08-08, `charges_duration: 31`).
//!
//!   * **grouped + charge-filter store read** — the store read
//!     (`Db::events_for_external_subscription_filtered`) now scopes by JSON
//!     property (charge/matching filter), and `aggregation::aggregate_grouped`
//!     splits by `grouped_by` property, mirroring the store's `GROUP BY g_*`
//!     queries. Numbers from the grouped/presentation cases of the same spec
//!     family.
//!
//! Every case inserts the exact event sequence into Postgres, reads it back
//! through the production repo scope, then runs the Rago aggregator and asserts
//! the exact `rust_decimal` value.

use chrono::{Duration, NaiveDate, NaiveDateTime, Utc};
use rago_billing::aggregation::{aggregate_grouped, ceil_dp, prorated_unique_count};
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
fn eod(y: i32, m: u32, d: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, m, d)
        .unwrap()
        .and_hms_opt(23, 59, 59)
        .unwrap()
}

fn metric(agg: AggregationType, field: Option<&str>, recurring: bool) -> BillableMetric {
    let now = Utc::now();
    BillableMetric {
        id: Uuid::new_v4(),
        organization_id: Uuid::new_v4(),
        name: "m".into(),
        code: CODE.into(),
        description: None,
        aggregation_type: agg,
        field_name: field.map(|s| s.to_string()),
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

// Recurring unique-count reads from history: fetch from a far-past boundary so
// events added before the period are included (Lago `use_from_boundary = false`).
fn history_from() -> NaiveDateTime {
    dt(2000, 1, 1, 0, 0, 0)
}
fn period_from() -> NaiveDateTime {
    dt(2022, 7, 9, 0, 0, 0)
}
fn period_to() -> NaiveDateTime {
    eod(2022, 8, 8)
}

async fn fetch(db: &rago_db::Db, org: Uuid) -> Vec<Event> {
    db.events_for_external_subscription(org, SUB, CODE, history_from(), period_to())
        .await
        .unwrap()
}

fn puc(events: &[Event]) -> Decimal {
    prorated_unique_count(events, "unique_id", period_from(), period_to(), 31).aggregation
}

// ---------------------------------------------------------------------------
// prorated_unique_count — prorated_aggregations/unique_count_service_spec.rb
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn prorated_unique_persisted_full_period(pool: PgPool) {
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    db.insert_event(&event(
        org,
        "a",
        dt(2022, 6, 9, 0, 0, 0),
        json!({"unique_id": "a"}),
    ))
    .await
    .unwrap();
    let events = fetch(&db, org).await;
    assert_eq!(puc(&events), dec!(1)); // Lago: 1
}

#[sqlx::test]
async fn prorated_unique_persisted_plus_added_in_period(pool: PgPool) {
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    db.insert_event(&event(
        org,
        "a",
        dt(2022, 6, 9, 0, 0, 0),
        json!({"unique_id": "a"}),
    ))
    .await
    .unwrap();
    db.insert_event(&event(
        org,
        "b",
        dt(2022, 7, 19, 0, 0, 0),
        json!({"unique_id": "b"}),
    ))
    .await
    .unwrap();
    let events = fetch(&db, org).await;
    // Lago: (1 + 21/31).ceil(5)
    assert_eq!(puc(&events), ceil_dp(dec!(1) + dec!(21) / dec!(31), 5));
}

#[sqlx::test]
async fn prorated_unique_added_mid_period(pool: PgPool) {
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    // Single add at from + 15 days (07-24).
    db.insert_event(&event(
        org,
        "a",
        dt(2022, 7, 24, 0, 0, 0),
        json!({"unique_id": "a"}),
    ))
    .await
    .unwrap();
    let events = fetch(&db, org).await;
    assert_eq!(puc(&events), ceil_dp(dec!(16) / dec!(31), 5)); // Lago: 16/31
}

#[sqlx::test]
async fn prorated_unique_removed_mid_period(pool: PgPool) {
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    db.insert_event(&event(
        org,
        "a",
        dt(2022, 6, 9, 0, 0, 0),
        json!({"unique_id": "a"}),
    ))
    .await
    .unwrap();
    db.insert_event(&event(
        org,
        "a_rm",
        dt(2022, 7, 24, 0, 0, 0),
        json!({"unique_id": "a", "operation_type": "remove"}),
    ))
    .await
    .unwrap();
    let events = fetch(&db, org).await;
    assert_eq!(puc(&events), ceil_dp(dec!(16) / dec!(31), 5)); // Lago: 16/31
}

#[sqlx::test]
async fn prorated_unique_added_and_removed_same_day_multiple(pool: PgPool) {
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let base = dt(2022, 7, 9, 1, 0, 0);
    let seq = [
        ("s0", base, "add"),
        ("s1", base + Duration::hours(1), "remove"),
        ("s2", base + Duration::hours(2), "add"),
        ("s3", base + Duration::hours(3), "remove"),
    ];
    for (txn, ts, op) in seq {
        db.insert_event(&event(
            org,
            txn,
            ts,
            json!({"unique_id": "a", "operation_type": op}),
        ))
        .await
        .unwrap();
    }
    let events = fetch(&db, org).await;
    assert_eq!(puc(&events), ceil_dp(dec!(1) / dec!(31), 5)); // Lago: 1/31
}

// ---------------------------------------------------------------------------
// grouped + charge-filter store read
// ---------------------------------------------------------------------------

#[sqlx::test]
async fn grouped_sum_over_real_event_store(pool: PgPool) {
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = eod(2023, 5, 31);

    // Three groups by `region` (eu=15, us=7, NULL=3).
    db.insert_event(&event(
        org,
        "e1",
        dt(2023, 5, 2, 0, 0, 0),
        json!({"amount": 10, "region": "eu"}),
    ))
    .await
    .unwrap();
    db.insert_event(&event(
        org,
        "e2",
        dt(2023, 5, 3, 0, 0, 0),
        json!({"amount": 5, "region": "eu"}),
    ))
    .await
    .unwrap();
    db.insert_event(&event(
        org,
        "e3",
        dt(2023, 5, 4, 0, 0, 0),
        json!({"amount": 7, "region": "us"}),
    ))
    .await
    .unwrap();
    db.insert_event(&event(
        org,
        "e4",
        dt(2023, 5, 5, 0, 0, 0),
        json!({"amount": 3}),
    ))
    .await
    .unwrap();

    // Unfiltered read → grouped aggregation.
    let events = db
        .events_for_external_subscription_filtered(org, SUB, CODE, from, to, &[])
        .await
        .unwrap();
    assert_eq!(events.len(), 4);

    let m = metric(AggregationType::SumAgg, Some("amount"), false);
    let r = aggregate_grouped(&m, &events, &["region".to_string()], 0, Decimal::ZERO).unwrap();
    assert_eq!(r.aggregation, dec!(25));
    assert_eq!(r.aggregations.len(), 3);
    let group = |v: Option<&str>| {
        r.aggregations
            .iter()
            .find(|a| a.grouped_by.get("region") == Some(&v.map(|s| s.to_string())))
            .unwrap()
            .aggregation
    };
    assert_eq!(group(Some("eu")), dec!(15));
    assert_eq!(group(Some("us")), dec!(7));
    assert_eq!(group(None), dec!(3));
}

#[sqlx::test]
async fn charge_filter_scopes_store_read(pool: PgPool) {
    let db = rago_db::Db::new(pool);
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = eod(2023, 5, 31);

    db.insert_event(&event(
        org,
        "e1",
        dt(2023, 5, 2, 0, 0, 0),
        json!({"amount": 10, "region": "eu"}),
    ))
    .await
    .unwrap();
    db.insert_event(&event(
        org,
        "e2",
        dt(2023, 5, 3, 0, 0, 0),
        json!({"amount": 5, "region": "eu"}),
    ))
    .await
    .unwrap();
    db.insert_event(&event(
        org,
        "e3",
        dt(2023, 5, 4, 0, 0, 0),
        json!({"amount": 7, "region": "us"}),
    ))
    .await
    .unwrap();

    // Charge/matching filter: region = eu → only the two eu events are in scope.
    let filters = vec![("region".to_string(), vec!["eu".to_string()])];
    let events = db
        .events_for_external_subscription_filtered(org, SUB, CODE, from, to, &filters)
        .await
        .unwrap();
    assert_eq!(
        events.len(),
        2,
        "charge filter scopes the store read to region=eu"
    );

    let m = metric(AggregationType::SumAgg, Some("amount"), false);
    let r = rago_billing::aggregation::aggregate(&m, &events, 0, Decimal::ZERO).unwrap();
    assert_eq!(r.aggregation, dec!(15)); // 10 + 5, us excluded
    assert_eq!(r.count, 2);
}
