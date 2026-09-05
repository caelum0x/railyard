//! ClickHouse SQL builder — pure functions (unit-tested), mirroring Lago's
//! `events_raw` schema and the `Events::Stores::ClickhouseStore` read scope.
//!
//! The scope is identical to the Postgres store (external_subscription_id +
//! organization_id + code + `[from?, to]` boundary, ordered by timestamp), so
//! the aggregators produce the same numbers regardless of backend.

use chrono::NaiveDateTime;

/// `CREATE TABLE events_raw` — the events table (Lago
/// `db/clickhouse_migrate/*_create_events_raw`).
///
/// Engine is `ReplacingMergeTree(ingested_at)`: rows sharing the sort key
/// `(organization_id, external_subscription_id, code, transaction_id, timestamp)`
/// are collapsed to the most-recently-ingested one. That sort key is exactly
/// Lago's ClickHouse dedup key (`DEDUP_KEY_COLUMNS` = code, organization_id,
/// external_subscription_id, transaction_id, timestamp), so a re-sent event is
/// metered once — the same idempotency the Postgres store gets from its unique
/// index on `(organization_id, external_subscription_id, transaction_id)`.
/// Deduplication is materialized at read time via `FINAL` (see
/// [`scope_select_sql`]); a plain `MergeTree` would double-count a re-sent
/// transaction_id and diverge from Lago/Postgres.
pub fn create_table_sql() -> &'static str {
    "CREATE TABLE IF NOT EXISTS events_raw (\
        organization_id String, \
        external_customer_id String, \
        external_subscription_id String, \
        transaction_id String, \
        timestamp DateTime64(3), \
        code String, \
        properties Map(String, String), \
        precise_total_amount_cents Nullable(Decimal(40, 15)), \
        ingested_at DateTime64(3) DEFAULT now64(3)\
    ) ENGINE = ReplacingMergeTree(ingested_at) ORDER BY (organization_id, external_subscription_id, code, transaction_id, timestamp)"
}

/// Escape a string for a ClickHouse single-quoted literal.
pub fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

fn fmt_ts(ts: NaiveDateTime) -> String {
    ts.format("%Y-%m-%d %H:%M:%S%.3f").to_string()
}

/// `Events::Stores::ClickhouseStore#events` — the boundary-scoped, ordered
/// read the aggregators consume, as a `JSONEachRow` SELECT.
///
/// `from` is applied only when the metric uses the from-boundary (non-recurring)
/// — the caller decides, matching `use_from_boundary`.
pub fn scope_select_sql(
    organization_id: &str,
    external_subscription_id: &str,
    code: &str,
    from: Option<NaiveDateTime>,
    to: NaiveDateTime,
) -> String {
    // `FINAL` forces ReplacingMergeTree deduplication at read time so a re-sent
    // transaction_id (same dedup key) is returned once — matching Lago's
    // `events_enriched` two-pass dedup and the Postgres store's unique index.
    let mut sql = format!(
        "SELECT organization_id, external_customer_id, external_subscription_id, transaction_id, \
         toString(timestamp) AS timestamp, code, \
         toJSONString(properties) AS properties, \
         toString(precise_total_amount_cents) AS precise_total_amount_cents \
         FROM events_raw FINAL \
         WHERE organization_id = '{}' AND external_subscription_id = '{}' AND code = '{}'",
        esc(organization_id),
        esc(external_subscription_id),
        esc(code),
    );
    if let Some(f) = from {
        sql.push_str(&format!(" AND timestamp >= '{}'", fmt_ts(f)));
    }
    sql.push_str(&format!(" AND timestamp <= '{}'", fmt_ts(to)));
    sql.push_str(" ORDER BY timestamp ASC FORMAT JSONEachRow");
    sql
}

/// An `INSERT … FORMAT JSONEachRow` statement for one event.
#[allow(clippy::too_many_arguments)]
pub fn insert_sql(
    organization_id: &str,
    external_customer_id: &str,
    external_subscription_id: &str,
    transaction_id: &str,
    timestamp: NaiveDateTime,
    code: &str,
    properties_json: &serde_json::Value,
    precise_total_amount_cents: Option<rust_decimal::Decimal>,
) -> String {
    // properties is a Map(String,String): stringify each value.
    let mut map = serde_json::Map::new();
    if let Some(obj) = properties_json.as_object() {
        for (k, v) in obj {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            map.insert(k.clone(), serde_json::Value::String(s));
        }
    }
    let row = serde_json::json!({
        "organization_id": organization_id,
        "external_customer_id": external_customer_id,
        "external_subscription_id": external_subscription_id,
        "transaction_id": transaction_id,
        "timestamp": fmt_ts(timestamp),
        "code": code,
        "properties": serde_json::Value::Object(map),
        "precise_total_amount_cents": precise_total_amount_cents.map(|d| d.to_string()),
    });
    format!("INSERT INTO events_raw FORMAT JSONEachRow\n{row}")
}
