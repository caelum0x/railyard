#![expect(clippy::expect_used, reason = "test helpers fail loudly on a broken scratch database")]

//! Live-Postgres proof of the per-key trace view and the retention job, run on
//! a scratch copy of the inference tables (a throwaway schema with the same
//! columns, partitioning and keys as the `tensorzero` tables).
//!
//! Skipped (reported as passing) unless `RELAY_TEST_DATABASE_URL` is set.

use relay_traces::query::{RawTraceQuery, TraceListParams, TraceScope};
use relay_traces::retention::{self, RetentionPolicy};
use relay_traces::store::{API_KEY_TAG, TraceStore};
use sqlx::postgres::PgPoolOptions;
use sqlx::{AssertSqlSafe, PgPool};
use uuid::Uuid;

fn db_url() -> Option<String> {
    std::env::var("RELAY_TEST_DATABASE_URL")
        .ok()
        .filter(|value| !value.trim().is_empty())
}

async fn exec(pool: &PgPool, sql: &str) {
    sqlx::raw_sql(AssertSqlSafe(sql.to_string()))
        .execute(pool)
        .await
        .expect("scratch DDL");
}

/// Create the scratch copy of the inference tables.
async fn create_copy(pool: &PgPool, schema: &str) {
    exec(
        pool,
        &format!("DROP SCHEMA IF EXISTS {schema} CASCADE; CREATE SCHEMA {schema};"),
    )
    .await;
    for table in ["chat_inferences", "json_inferences"] {
        exec(
            pool,
            &format!(
                "CREATE TABLE {schema}.{table} (id UUID NOT NULL, function_name TEXT NOT NULL, \
                 variant_name TEXT NOT NULL, episode_id UUID NOT NULL, processing_time_ms INTEGER, \
                 ttft_ms INTEGER, tags JSONB NOT NULL DEFAULT '{{}}', created_at TIMESTAMPTZ NOT NULL, \
                 PRIMARY KEY (id, created_at)) PARTITION BY RANGE (created_at); \
                 CREATE TABLE {schema}.{table}_default PARTITION OF {schema}.{table} DEFAULT; \
                 CREATE INDEX ON {schema}.{table} USING GIN (tags);"
            ),
        )
        .await;
    }
    exec(
        pool,
        &format!(
            "CREATE TABLE {schema}.model_inferences (id UUID NOT NULL, inference_id UUID NOT NULL, \
             input_tokens INTEGER, output_tokens INTEGER, response_time_ms INTEGER, \
             model_name TEXT NOT NULL, model_provider_name TEXT NOT NULL, ttft_ms INTEGER, \
             cached BOOLEAN NOT NULL DEFAULT false, finish_reason TEXT, cost NUMERIC(18, 9), \
             created_at TIMESTAMPTZ NOT NULL, PRIMARY KEY (id, created_at)) \
             PARTITION BY RANGE (created_at); \
             CREATE TABLE {schema}.model_inferences_default PARTITION OF {schema}.model_inferences DEFAULT;"
        ),
    )
    .await;
    for table in ["chat_inference_data", "json_inference_data"] {
        exec(
            pool,
            &format!(
                "CREATE TABLE {schema}.{table} (id UUID NOT NULL, input JSONB NOT NULL, \
                 output JSONB NOT NULL, created_at TIMESTAMPTZ NOT NULL, PRIMARY KEY (id, created_at)) \
                 PARTITION BY RANGE (created_at); \
                 CREATE TABLE {schema}.{table}_default PARTITION OF {schema}.{table} DEFAULT;"
            ),
        )
        .await;
    }
    exec(
        pool,
        &format!(
            "CREATE TABLE {schema}.model_inference_data (id UUID NOT NULL, raw_request TEXT NOT NULL, \
             raw_response TEXT NOT NULL, created_at TIMESTAMPTZ NOT NULL, PRIMARY KEY (id, created_at)) \
             PARTITION BY RANGE (created_at); \
             CREATE TABLE {schema}.model_inference_data_default PARTITION OF {schema}.model_inference_data DEFAULT;"
        ),
    )
    .await;
}

/// Insert one chat trace for `key`, `age_days` old, with two model calls (a
/// failed first provider, then the answering one).
async fn insert_trace(pool: &PgPool, schema: &str, key: &str, age_days: i32) -> Uuid {
    let id = Uuid::now_v7();
    let at = format!("NOW() - make_interval(days => {age_days})");
    sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO {schema}.chat_inferences (id, function_name, variant_name, episode_id, \
         processing_time_ms, ttft_ms, tags, created_at) VALUES ($1, 'draft_email', 'v1', $2, 812, 90, \
         jsonb_build_object($3::text, $4::text), {at})"
    )))
    .bind(id)
    .bind(Uuid::now_v7())
    .bind(API_KEY_TAG)
    .bind(key)
    .execute(pool)
    .await
    .expect("insert chat inference");
    sqlx::query(AssertSqlSafe(format!(
        "INSERT INTO {schema}.chat_inference_data (id, input, output, created_at) \
         VALUES ($1, '{{\"messages\":[\"hi\"]}}', '[{{\"text\":\"hello\"}}]', {at})"
    )))
    .bind(id)
    .execute(pool)
    .await
    .expect("insert chat body");
    for (offset_ms, model, provider, cost, finish) in [
        (1, "claude-sonnet", "openrouter", None, "unknown"),
        (2, "claude-sonnet", "anthropic", Some("0.001250000"), "stop"),
    ] {
        let model_id = Uuid::now_v7();
        sqlx::query(AssertSqlSafe(format!(
            "INSERT INTO {schema}.model_inferences (id, inference_id, input_tokens, output_tokens, \
             response_time_ms, model_name, model_provider_name, ttft_ms, cached, finish_reason, cost, \
             created_at) VALUES ($1, $2, 100, 50, 400, $3, $4, 80, false, $5, $6::numeric, \
             {at} + make_interval(secs => {offset_ms} / 1000.0))"
        )))
        .bind(model_id)
        .bind(id)
        .bind(model)
        .bind(provider)
        .bind(finish)
        .bind(cost)
        .execute(pool)
        .await
        .expect("insert model inference");
        sqlx::query(AssertSqlSafe(format!(
            "INSERT INTO {schema}.model_inference_data (id, raw_request, raw_response, created_at) \
             VALUES ($1, 'req', 'resp', {at})"
        )))
        .bind(model_id)
        .execute(pool)
        .await
        .expect("insert model body");
    }
    id
}

fn params(limit: i64, before: Option<String>) -> TraceListParams {
    TraceListParams::from_raw(&RawTraceQuery {
        limit: Some(limit),
        before,
        ..Default::default()
    })
    .expect("valid params")
}

#[tokio::test]
async fn per_key_view_isolation_pagination_and_retention() {
    let Some(url) = db_url() else {
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(&url)
        .await
        .expect("connect to the live test Postgres");
    let schema = format!("relay_traces_test_{}", Uuid::now_v7().simple());
    create_copy(&pool, &schema).await;
    let store = TraceStore::with_schema(pool.clone(), &schema).expect("valid schema");
    let key_a = "key_a_public";
    let key_b = "key_b_public";

    let a1 = insert_trace(&pool, &schema, key_a, 0).await;
    let a2 = insert_trace(&pool, &schema, key_a, 0).await;
    let a3 = insert_trace(&pool, &schema, key_a, 0).await;
    let b1 = insert_trace(&pool, &schema, key_b, 0).await;

    // Key A sees its own traces, newest first, with model, tokens, cost.
    let page = store
        .list(&TraceScope::Key(key_a.into()), &params(50, None))
        .await
        .expect("list A");
    let ids: Vec<Uuid> = page.traces.iter().map(|t| t.inference_id).collect();
    assert_eq!(
        ids,
        vec![a3, a2, a1],
        "key A sees exactly its traces, newest first"
    );
    let first = &page.traces[0];
    assert_eq!(first.model_name.as_deref(), Some("claude-sonnet"));
    assert_eq!(
        first.model_provider_name.as_deref(),
        Some("anthropic"),
        "last call answered"
    );
    assert_eq!(first.finish_reason.as_deref(), Some("stop"));
    assert_eq!(first.input_tokens, Some(200));
    assert_eq!(first.output_tokens, Some(100));
    assert_eq!(first.cost_micros, Some(1250));
    assert_eq!(first.model_calls, 2);
    assert_eq!(first.processing_time_ms, Some(812));
    assert_eq!(first.api_key_public_id.as_deref(), Some(key_a));
    assert_eq!(page.next_before, None);

    // Key B cannot see A's traces: empty list filter and `None` detail (404).
    let page_b = store
        .list(&TraceScope::Key(key_b.into()), &params(50, None))
        .await
        .expect("list B");
    assert_eq!(
        page_b
            .traces
            .iter()
            .map(|t| t.inference_id)
            .collect::<Vec<_>>(),
        vec![b1]
    );
    assert!(
        store
            .get(&TraceScope::Key(key_b.into()), a1)
            .await
            .expect("get as B")
            .is_none(),
        "key B must get nothing for key A's trace"
    );

    // Key A's detail carries both model calls and the bodies.
    let detail = store
        .get(&TraceScope::Key(key_a.into()), a1)
        .await
        .expect("get as A")
        .expect("A owns a1");
    assert_eq!(detail.model_calls.len(), 2);
    assert!(!detail.bodies_expired);
    assert_eq!(
        detail.output,
        Some(serde_json::json!([{ "text": "hello" }]))
    );

    // Operator scope sees all keys, or narrows to one.
    let all = store
        .list(&TraceScope::All { key: None }, &params(50, None))
        .await
        .expect("list all");
    assert_eq!(all.traces.len(), 4);
    let only_b = store
        .list(
            &TraceScope::All {
                key: Some(key_b.into()),
            },
            &params(50, None),
        )
        .await
        .expect("list all narrowed to B");
    assert_eq!(only_b.traces.len(), 1);

    // Keyset pagination.
    let page1 = store
        .list(&TraceScope::Key(key_a.into()), &params(2, None))
        .await
        .expect("page 1");
    assert_eq!(page1.traces.len(), 2);
    let cursor = page1.next_before.clone().expect("a next page exists");
    let page2 = store
        .list(&TraceScope::Key(key_a.into()), &params(2, Some(cursor)))
        .await
        .expect("page 2");
    assert_eq!(
        page2
            .traces
            .iter()
            .map(|t| t.inference_id)
            .collect::<Vec<_>>(),
        vec![a1]
    );

    // Retention on the copy: a 20-day-old trace loses its bodies but keeps its
    // metadata; a 100-day-old trace is removed entirely; fresh rows stay.
    let old20 = insert_trace(&pool, &schema, key_a, 20).await;
    let old100 = insert_trace(&pool, &schema, key_a, 100).await;
    let policy = RetentionPolicy::from_values(None, None, true).expect("default policy");
    let dry = retention::apply(&pool, &schema, &policy)
        .await
        .expect("dry run");
    let rows = |report: &retention::RetentionReport, table: &str| {
        report
            .tables
            .iter()
            .find(|t| t.table == table)
            .map(|t| t.rows)
            .expect("table in report")
    };
    assert_eq!(
        rows(&dry, "chat_inference_data"),
        2,
        "20d and 100d bodies are over 14 days"
    );
    assert_eq!(rows(&dry, "model_inference_data"), 4);
    assert_eq!(
        rows(&dry, "chat_inferences"),
        1,
        "only the 100d row is over 90 days"
    );
    assert_eq!(rows(&dry, "model_inferences"), 2);
    assert!(
        dry.tables
            .iter()
            .any(|t| t.table == "json_inference_data" && t.rows == 0)
    );

    // A dry run deleted nothing.
    let still = store
        .get(&TraceScope::Key(key_a.into()), old20)
        .await
        .expect("get old20")
        .expect("old20 exists");
    assert!(!still.bodies_expired);

    let policy = RetentionPolicy {
        batch_size: 1,
        ..RetentionPolicy::from_values(None, None, false).expect("default policy")
    };
    let applied = retention::apply(&pool, &schema, &policy)
        .await
        .expect("apply");
    assert_eq!(
        applied.total_rows(),
        dry.total_rows(),
        "apply removes what the dry run counted"
    );

    let aged = store
        .get(&TraceScope::Key(key_a.into()), old20)
        .await
        .expect("get old20 after retention")
        .expect("metadata kept inside 90 days");
    assert!(aged.bodies_expired, "bodies older than 14 days are gone");
    assert_eq!(aged.model_calls.len(), 2, "model-call metadata kept");
    let left: i64 = sqlx::query_scalar(AssertSqlSafe(format!(
        "SELECT (SELECT COUNT(*) FROM {schema}.chat_inferences WHERE id = $1) \
         + (SELECT COUNT(*) FROM {schema}.model_inferences WHERE inference_id = $1)"
    )))
    .bind(old100)
    .fetch_one(&pool)
    .await
    .expect("count old100 rows");
    assert_eq!(left, 0, "metadata older than 90 days is gone");
    let fresh = store
        .get(&TraceScope::Key(key_a.into()), a1)
        .await
        .expect("get a1 after retention")
        .expect("fresh trace kept");
    assert!(!fresh.bodies_expired, "fresh bodies untouched");

    exec(&pool, &format!("DROP SCHEMA {schema} CASCADE;")).await;
}
