//! Live-ClickHouse aggregator parity suite.
//!
//! The ClickHouse twin of `rago-db/tests/aggregator_parity.rs`. It proves that
//! Rago's **ClickHouse** event-store path (`rago-clickhouse`) feeds the same
//! `rago-billing` aggregators the same events the Postgres store does, and so
//! produces the exact numbers Lago asserts — over a *real* ClickHouse server,
//! not a hand-built `&[Event]`.
//!
//! Each test inserts the exact event sequence from a Lago aggregation spec into
//! `events_raw` via [`ClickHouseStore::insert_event`], reads it back through the
//! production read scope ([`ClickHouseStore::events_in_scope`] =
//! `Events::Stores::ClickhouseStore#events`), runs Rago's aggregator over the
//! fetched events, and asserts the aggregate equals Lago's asserted value
//! (exact `rust_decimal`). The Lago spec each case maps to is cited inline, and
//! the asserted numbers are identical to the Postgres suite — proving the CH and
//! PG paths are numerically indistinguishable.
//!
//! Deduplication note: unlike Postgres (unique index → insert is a no-op),
//! ClickHouse ingests every row. Rago's `events_raw` is a
//! `ReplacingMergeTree(ingested_at)` whose sort key is Lago's ClickHouse dedup
//! key, and the read scope selects `FROM events_raw FINAL`, so a re-sent
//! transaction_id is metered once — matching Lago's `events_enriched` two-pass
//! dedup. The `*_dedups_duplicate_transaction` cases exercise this live.
//!
//! Gating: the suite is skipped entirely unless `CLICKHOUSE_URL` is set (e.g.
//! `http://localhost:8123`), so DB-less CI passes. Optional
//! `CLICKHOUSE_DATABASE` / `CLICKHOUSE_USER` / `CLICKHOUSE_PASSWORD` default to
//! `default` / `default` / empty. Tests share one `events_raw` table and isolate
//! by using a fresh `organization_id` per test (as the Postgres suite does).

use chrono::{NaiveDate, NaiveDateTime, Utc};
use rago_billing::aggregation::{aggregate, prorated_sum, weighted_sum};
use rago_clickhouse::ClickHouseStore;
use rago_core::enums::AggregationType;
use rago_core::models::{BillableMetric, Event};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::{json, Value};
use uuid::Uuid;

const CODE: &str = "bm_code";
const SUB: &str = "sub_ext_1";

/// Build a store from `CLICKHOUSE_URL` (+ optional creds) and ensure the table
/// exists. Returns `None` when `CLICKHOUSE_URL` is unset so the test skips.
async fn store() -> Option<ClickHouseStore> {
    let url = std::env::var("CLICKHOUSE_URL").ok()?;
    let db = std::env::var("CLICKHOUSE_DATABASE").unwrap_or_else(|_| "default".into());
    let user = std::env::var("CLICKHOUSE_USER").unwrap_or_else(|_| "default".into());
    let pass = std::env::var("CLICKHOUSE_PASSWORD").unwrap_or_default();
    let store = ClickHouseStore::new(url, db, user, pass);
    store
        .migrate()
        .await
        .expect("CLICKHOUSE_URL is set but migrate() failed");
    Some(store)
}

/// Skip-guard: bind a live store or `return` (skips the test) with a note.
macro_rules! live_store {
    () => {
        match store().await {
            Some(s) => s,
            None => {
                eprintln!("skipping live ClickHouse parity test: CLICKHOUSE_URL not set");
                return;
            }
        }
    };
}

fn dt(y: i32, m: u32, d: u32, hh: u32, mm: u32, ss: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, m, d)
        .unwrap()
        .and_hms_opt(hh, mm, ss)
        .unwrap()
}

/// Lago's `to_datetime` = `end_of_day` = `23:59:59.999`.
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
        external_customer_id: Some("cust".into()),
        external_subscription_id: Some(SUB.to_string()),
        timestamp: Some(ts),
        precise_total_amount_cents: None,
        created_at: now,
        updated_at: now,
        deleted_at: None,
        persisted: true,
    }
}

async fn insert(store: &ClickHouseStore, e: &Event) {
    store.insert_event(e, "cust").await.expect("insert_event");
}

/// Read scope the aggregators consume (`Events::Stores::ClickhouseStore#events`).
/// `from` is `None` for recurring metrics (full history), `Some` otherwise.
async fn fetch(
    store: &ClickHouseStore,
    org: Uuid,
    from: Option<NaiveDateTime>,
    to: NaiveDateTime,
) -> Vec<Event> {
    store
        .events_in_scope(org, SUB, CODE, from, to)
        .await
        .expect("events_in_scope")
}

fn as_ts_values(events: &[Event], field: &str) -> Vec<(NaiveDateTime, Decimal)> {
    events
        .iter()
        .map(|e| (e.timestamp.unwrap(), e.field_value(field)))
        .collect()
}

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

#[tokio::test]
async fn sum_agg_matches_lago() {
    let store = live_store!();
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);

    for i in 0..2 {
        insert(
            &store,
            &event(
                org,
                &format!("old{i}"),
                dt(2023, 3, 1, 0, 0, 0),
                json!({"total_count": 2.5}),
            ),
        )
        .await;
    }
    for i in 0..4 {
        insert(
            &store,
            &event(
                org,
                &format!("new{i}"),
                dt(2023, 5, 30, 0, 0, 0),
                json!({"total_count": 12}),
            ),
        )
        .await;
    }

    let events = fetch(&store, org, Some(from), to).await;
    assert_eq!(
        events.len(),
        4,
        "from-boundary excludes the 2 out-of-period events"
    );

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

#[tokio::test]
async fn sum_agg_recurring_uses_full_history() {
    // Recurring ⇒ no from-boundary (from = None): read all history.
    let store = live_store!();
    let org = Uuid::new_v4();
    let to = dt(2023, 5, 31, 23, 59, 59);

    for i in 0..2 {
        insert(
            &store,
            &event(
                org,
                &format!("old{i}"),
                dt(2023, 3, 1, 0, 0, 0),
                json!({"total_count": 2.5}),
            ),
        )
        .await;
    }
    for i in 0..4 {
        insert(
            &store,
            &event(
                org,
                &format!("new{i}"),
                dt(2023, 5, 30, 0, 0, 0),
                json!({"total_count": 12}),
            ),
        )
        .await;
    }

    let events = fetch(&store, org, None, to).await;
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

#[tokio::test]
async fn sum_agg_out_of_bounds_is_zero() {
    let store = live_store!();
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);
    for i in 0..4 {
        insert(
            &store,
            &event(
                org,
                &format!("oob{i}"),
                dt(2023, 6, 1, 0, 0, 0),
                json!({"total_count": 12}),
            ),
        )
        .await;
    }
    let events = fetch(&store, org, Some(from), to).await;
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

#[tokio::test]
async fn sum_agg_float_and_filter() {
    let store = live_store!();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);

    // "when properties is a float": 4×12 + 4.5 = 52.5
    let org = Uuid::new_v4();
    for i in 0..4 {
        insert(
            &store,
            &event(
                org,
                &format!("f{i}"),
                dt(2023, 5, 20, 0, 0, 0),
                json!({"total_count": 12}),
            ),
        )
        .await;
    }
    insert(
        &store,
        &event(
            org,
            "ff",
            dt(2023, 5, 21, 0, 0, 0),
            json!({"total_count": 4.5}),
        ),
    )
    .await;
    let events = fetch(&store, org, Some(from), to).await;
    let r = aggregate(
        &metric(AggregationType::SumAgg, Some("total_count")),
        &events,
        0,
        dec!(0),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(52.5)); // Lago: 52.5

    // "when filters are given" region=europe: 12 + 8 = 20, count 2.
    // The two europe events carry distinct timestamps: the read scope orders by
    // `timestamp ASC` only (no tie-break — identical in Lago, the Postgres store
    // and here), so equal timestamps would leave `running_total` order undefined
    // under ClickHouse. Distinct timestamps make the 12-then-8 order — and thus
    // the running_total — deterministic without diverging from the scope.
    let org2 = Uuid::new_v4();
    insert(
        &store,
        &event(
            org2,
            "e1",
            dt(2023, 5, 20, 0, 0, 0),
            json!({"total_count": 12, "region": "europe"}),
        ),
    )
    .await;
    insert(
        &store,
        &event(
            org2,
            "e2",
            dt(2023, 5, 20, 1, 0, 0),
            json!({"total_count": 8, "region": "europe"}),
        ),
    )
    .await;
    insert(
        &store,
        &event(
            org2,
            "a1",
            dt(2023, 5, 20, 0, 0, 0),
            json!({"total_count": 12, "region": "africa"}),
        ),
    )
    .await;
    let all = fetch(&store, org2, Some(from), to).await;
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
}

#[tokio::test]
async fn sum_agg_dedups_duplicate_transaction() {
    // ClickHouse ingests every row; ReplacingMergeTree + FINAL collapses the
    // re-sent transaction_id so the aggregator meters it once (48, not 60).
    let store = live_store!();
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);
    for i in 0..4 {
        insert(
            &store,
            &event(
                org,
                &format!("t{i}"),
                dt(2023, 5, 20, 0, 0, 0),
                json!({"total_count": 12}),
            ),
        )
        .await;
    }
    // Re-send transaction t0 (same dedup key) — must collapse.
    insert(
        &store,
        &event(
            org,
            "t0",
            dt(2023, 5, 20, 0, 0, 0),
            json!({"total_count": 12}),
        ),
    )
    .await;

    let events = fetch(&store, org, Some(from), to).await;
    assert_eq!(
        events.len(),
        4,
        "duplicate transaction_id returned once (FINAL)"
    );
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

#[tokio::test]
async fn count_agg_matches_lago() {
    let store = live_store!();
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);
    for i in 0..4 {
        insert(
            &store,
            &event(org, &format!("c{i}"), dt(2023, 5, 20, 0, 0, 0), json!({})),
        )
        .await;
    }
    let events = fetch(&store, org, Some(from), to).await;
    let r = aggregate(
        &metric(AggregationType::CountAgg, None),
        &events,
        0,
        dec!(0),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(4)); // Lago: 4
}

#[tokio::test]
async fn count_agg_dedups_duplicate_transaction() {
    // count_service_spec.rb ClickHouse "with deduplication": 4 events sharing a
    // transaction_id count as 1 — here materialized live via FINAL.
    let store = live_store!();
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);
    for _ in 0..4 {
        insert(
            &store,
            &event(org, "123456", dt(2023, 5, 20, 0, 0, 0), json!({})),
        )
        .await;
    }
    let events = fetch(&store, org, Some(from), to).await;
    assert_eq!(events.len(), 1, "shared transaction_id deduped by FINAL");
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

#[tokio::test]
async fn max_agg_matches_lago() {
    let store = live_store!();
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);
    for (i, v) in [3, 5, 2, 7].iter().enumerate() {
        insert(
            &store,
            &event(
                org,
                &format!("m{i}"),
                dt(2023, 5, 10, 0, 0, 0),
                json!({"total_count": v}),
            ),
        )
        .await;
    }
    insert(
        &store,
        &event(
            org,
            "mmax",
            dt(2023, 5, 11, 0, 0, 0),
            json!({"total_count": 12}),
        ),
    )
    .await;
    let events = fetch(&store, org, Some(from), to).await;
    let r = aggregate(
        &metric(AggregationType::MaxAgg, Some("total_count")),
        &events,
        0,
        dec!(0),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(12)); // Lago: 12
    assert_eq!(r.count, 5);
}

// ---------------------------------------------------------------------------
// latest_agg — latest_service_spec.rb
// ---------------------------------------------------------------------------

#[tokio::test]
async fn latest_agg_matches_lago() {
    let store = live_store!();
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);
    for i in 0..4 {
        insert(
            &store,
            &event(
                org,
                &format!("l{i}"),
                dt(2023, 5, 10, 0, 0, 0),
                json!({"total_count": 18}),
            ),
        )
        .await;
    }
    insert(
        &store,
        &event(
            org,
            "llast",
            dt(2023, 5, 11, 0, 0, 0),
            json!({"total_count": 14}),
        ),
    )
    .await;
    let events = fetch(&store, org, Some(from), to).await;
    let r = aggregate(
        &metric(AggregationType::LatestAgg, Some("total_count")),
        &events,
        0,
        dec!(0),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(14)); // Lago: 14 (last by timestamp)
    assert_eq!(r.count, 5);
}

// ---------------------------------------------------------------------------
// unique_count_agg — unique_count_service_spec.rb
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unique_count_recurring_vs_non_recurring() {
    let store = live_store!();
    let org = Uuid::new_v4();
    let period_from = dt(2022, 7, 9, 0, 0, 0);
    let to = dt(2022, 8, 8, 23, 59, 59);

    insert(
        &store,
        &event(
            org,
            "persisted",
            dt(2022, 6, 9, 0, 0, 0),
            json!({"unique_id": "aaa"}),
        ),
    )
    .await;
    insert(
        &store,
        &event(
            org,
            "inperiod",
            dt(2022, 7, 19, 0, 0, 0),
            json!({"unique_id": "bbb"}),
        ),
    )
    .await;

    // Recurring ⇒ from = None (full history) ⇒ 2.
    let recurring = fetch(&store, org, None, to).await;
    let r = aggregate(
        &metric(AggregationType::UniqueCountAgg, Some("unique_id")),
        &recurring,
        0,
        dec!(0),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(2)); // Lago recurring: 2

    // Non-recurring ⇒ from-boundary ⇒ only the in-period event ⇒ 1.
    let non_recurring = fetch(&store, org, Some(period_from), to).await;
    let r = aggregate(
        &metric(AggregationType::UniqueCountAgg, Some("unique_id")),
        &non_recurring,
        0,
        dec!(0),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(1)); // Lago non-recurring: 1
}

#[tokio::test]
async fn unique_count_add_then_remove_nets_zero() {
    let store = live_store!();
    let org = Uuid::new_v4();
    let to = dt(2022, 8, 8, 23, 59, 59);

    insert(
        &store,
        &event(
            org,
            "add",
            dt(2022, 6, 9, 0, 0, 0),
            json!({"unique_id": "aaa"}),
        ),
    )
    .await;
    insert(
        &store,
        &event(
            org,
            "rem",
            dt(2022, 7, 24, 0, 0, 0),
            json!({"unique_id": "aaa", "operation_type": "remove"}),
        ),
    )
    .await;

    let events = fetch(&store, org, None, to).await;
    let r = aggregate(
        &metric(AggregationType::UniqueCountAgg, Some("unique_id")),
        &events,
        0,
        dec!(0),
    )
    .unwrap();
    assert_eq!(r.aggregation, dec!(0)); // Lago: 0 (remove cancels the add)
}

// ---------------------------------------------------------------------------
// weighted_sum_agg — weighted_sum_service_spec.rb
// ---------------------------------------------------------------------------

#[tokio::test]
async fn weighted_sum_time_weighting() {
    let store = live_store!();
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
    for (i, (ts, v)) in seq.iter().enumerate() {
        insert(
            &store,
            &event(org, &format!("w{i}"), *ts, json!({"value": v})),
        )
        .await;
    }
    let events = fetch(&store, org, Some(from), to).await;
    let vals = as_ts_values(&events, "value");
    let r = weighted_sum(&vals, from, to, 31, dec!(0));
    assert_eq!(r.aggregation.round_dp(5), dec!(0.02218)); // Lago: 0.02218
    assert_eq!(r.count, 7);
}

#[tokio::test]
async fn weighted_sum_single_event_full_period() {
    let store = live_store!();
    let org = Uuid::new_v4();
    let from = dt(2023, 8, 1, 0, 0, 0);
    let to = end_of_day(2023, 8, 31);
    insert(
        &store,
        &event(org, "solo", dt(2023, 8, 1, 0, 0, 0), json!({"value": 1000})),
    )
    .await;
    let events = fetch(&store, org, Some(from), to).await;
    let vals = as_ts_values(&events, "value");
    let r = weighted_sum(&vals, from, to, 31, dec!(0));
    assert_eq!(r.aggregation.round_dp(5), dec!(1000.0)); // Lago: 1000.0
    assert_eq!(r.count, 1);
}

// ---------------------------------------------------------------------------
// prorated sum — prorated_aggregations/sum_service_spec.rb
// ---------------------------------------------------------------------------

#[tokio::test]
async fn prorated_sum_within_period() {
    let store = live_store!();
    let org = Uuid::new_v4();
    let from = dt(2023, 5, 1, 0, 0, 0);
    let to = dt(2023, 5, 31, 23, 59, 59);
    for i in 0..2 {
        insert(
            &store,
            &event(
                org,
                &format!("p{i}"),
                dt(2023, 5, 26, 0, 0, 0),
                json!({"total_count": 12}),
            ),
        )
        .await;
    }
    let events = fetch(&store, org, Some(from), to).await;
    let pairs: Vec<(NaiveDate, Decimal)> = events
        .iter()
        .map(|e| (e.timestamp.unwrap().date(), e.field_value("total_count")))
        .collect();
    let r = prorated_sum(&pairs, to.date(), 31);
    assert_eq!(r.aggregation, dec!(4.64517)); // Lago: 2 × 12*6/31, ceil5
    assert_eq!(r.count, 2);
}
