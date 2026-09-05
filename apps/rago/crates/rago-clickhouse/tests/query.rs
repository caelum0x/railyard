//! ClickHouse SQL-builder unit tests (no live server required).

use chrono::NaiveDate;
use rago_clickhouse::query;

fn dt(y: i32, m: u32, d: u32, h: u32, mi: u32, s: u32) -> chrono::NaiveDateTime {
    NaiveDate::from_ymd_opt(y, m, d)
        .unwrap()
        .and_hms_opt(h, mi, s)
        .unwrap()
}

#[test]
fn create_table_has_replacing_mergetree_and_order() {
    let sql = query::create_table_sql();
    assert!(sql.contains("events_raw"));
    // ReplacingMergeTree(ingested_at) dedups re-sent transaction_ids (Lago parity).
    assert!(sql.contains("ENGINE = ReplacingMergeTree(ingested_at)"));
    // Sort key == Lago's DEDUP_KEY_COLUMNS set (code, org, sub, transaction_id, timestamp).
    assert!(sql.contains(
        "ORDER BY (organization_id, external_subscription_id, code, transaction_id, timestamp)"
    ));
    assert!(sql.contains("properties Map(String, String)"));
}

#[test]
fn scope_select_matches_postgres_where() {
    let from = dt(2024, 1, 1, 0, 0, 0);
    let to = dt(2024, 1, 31, 23, 59, 59);
    let sql = query::scope_select_sql("org1", "sub1", "api_calls", Some(from), to);
    // FINAL forces ReplacingMergeTree dedup at read time.
    assert!(sql.contains("FROM events_raw FINAL"));
    assert!(sql.contains("WHERE organization_id = 'org1'"));
    assert!(sql.contains("AND external_subscription_id = 'sub1'"));
    assert!(sql.contains("AND code = 'api_calls'"));
    assert!(sql.contains("AND timestamp >= '2024-01-01 00:00:00.000'"));
    assert!(sql.contains("AND timestamp <= '2024-01-31 23:59:59.000'"));
    assert!(sql.contains("ORDER BY timestamp ASC"));
    assert!(sql.ends_with("FORMAT JSONEachRow"));
}

#[test]
fn scope_select_without_from_boundary() {
    let to = dt(2024, 2, 1, 0, 0, 0);
    let sql = query::scope_select_sql("o", "s", "c", None, to);
    assert!(!sql.contains("timestamp >="));
    assert!(sql.contains("timestamp <="));
}

#[test]
fn insert_is_json_each_row() {
    let ts = dt(2024, 1, 5, 12, 0, 0);
    let props = serde_json::json!({ "value": 40 });
    let sql = query::insert_sql(
        "org1",
        "cust1",
        "sub1",
        "tx1",
        ts,
        "api_calls",
        &props,
        None,
    );
    assert!(sql.starts_with("INSERT INTO events_raw FORMAT JSONEachRow\n"));
    let row: serde_json::Value = serde_json::from_str(sql.splitn(2, '\n').nth(1).unwrap()).unwrap();
    assert_eq!(row["organization_id"], "org1");
    assert_eq!(row["code"], "api_calls");
    // Map(String,String): the numeric value is stringified.
    assert_eq!(row["properties"]["value"], "40");
    assert_eq!(row["timestamp"], "2024-01-05 12:00:00.000");
}

#[test]
fn escaping() {
    assert_eq!(query::esc("a'b"), "a\\'b");
}
