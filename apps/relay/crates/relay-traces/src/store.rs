//! Postgres reads behind `GET /relay/traces` and `GET /relay/traces/{id}`.
//!
//! Reads only; this module never writes. Every query is bounded by a time
//! window so Postgres prunes the monthly (metadata) and daily (body)
//! partitions, and the key filter uses the existing GIN index on `tags`
//! (`tags @> {"tensorzero::api_key_public_id": ...}`).

use serde::Serialize;
use sqlx::{PgPool, Postgres, QueryBuilder, Row, postgres::PgRow};
use uuid::Uuid;

use crate::query::{MAX_WINDOW_HOURS, TraceCursor, TraceListParams, TraceScope};
use crate::valid_schema_name;

/// Schema the gateway writes inferences into.
pub const DEFAULT_SCHEMA: &str = "tensorzero";
/// Tag the gateway stamps on every authenticated inference.
pub const API_KEY_TAG: &str = "tensorzero::api_key_public_id";

#[derive(Debug, thiserror::Error)]
pub enum TraceStoreError {
    #[error("invalid schema name `{0}`")]
    InvalidSchema(String),
    #[error("trace query failed: {0}")]
    Database(#[from] sqlx::Error),
}

/// One row of the trace list: an inference plus its model calls rolled up.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TraceSummary {
    pub inference_id: Uuid,
    pub episode_id: Uuid,
    /// `chat` or `json`.
    pub kind: String,
    pub function_name: String,
    pub variant_name: String,
    /// `None` for unauthenticated traffic (operator scope only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_key_public_id: Option<String>,
    pub created_at_us: i64,
    /// Model of the last model call (the one that answered).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_provider_name: Option<String>,
    /// Finish reason of the last model call; `None` when no call was stored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i64>,
    /// Cost in micro-USD; `None` when the provider price is unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_micros: Option<i64>,
    /// End-to-end gateway time.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub processing_time_ms: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<i32>,
    /// Number of model calls (retries, fallbacks and best-of-n candidates).
    pub model_calls: i64,
    pub cached: bool,
}

/// One model call inside a trace.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelCall {
    pub id: Uuid,
    pub model_name: String,
    pub model_provider_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub response_time_ms: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_micros: Option<i64>,
    pub cached: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    pub created_at_us: i64,
}

/// Full trace: summary, every model call, and the stored bodies when they are
/// still inside the body-retention window.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TraceDetail {
    pub summary: TraceSummary,
    pub model_calls: Vec<ModelCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<serde_json::Value>,
    /// True when the metadata exists but the bodies were removed by retention.
    pub bodies_expired: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TracePage {
    pub traces: Vec<TraceSummary>,
    /// Pass back as `before` for the next page; `None` on the last page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_before: Option<String>,
    pub window_hours: i64,
    /// The key the page is scoped to (`None` for the operator-wide view).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

/// Postgres trace reader.
#[derive(Clone)]
pub struct TraceStore {
    pool: PgPool,
    schema: String,
}

/// Which inference table a list branch reads.
#[derive(Clone, Copy)]
enum Branch {
    Chat,
    Json,
}

impl TraceStore {
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            schema: DEFAULT_SCHEMA.to_string(),
        }
    }

    /// Reader over another schema (used by the tests on a scratch copy).
    pub fn with_schema(pool: PgPool, schema: &str) -> Result<Self, TraceStoreError> {
        if !valid_schema_name(schema) {
            return Err(TraceStoreError::InvalidSchema(schema.to_string()));
        }
        Ok(Self {
            pool,
            schema: schema.to_string(),
        })
    }

    /// List traces newest first.
    pub async fn list(
        &self,
        scope: &TraceScope,
        params: &TraceListParams,
    ) -> Result<TracePage, TraceStoreError> {
        let rows = self.summaries(scope, params, None).await?;
        let next_before = (rows.len() as i64 == params.limit)
            .then(|| rows.last())
            .flatten()
            .map(|last| {
                TraceCursor {
                    created_at_us: last.created_at_us,
                    id: last.inference_id,
                }
                .render()
            });
        Ok(TracePage {
            traces: rows,
            next_before,
            window_hours: params.window_hours,
            key: scope.key_filter().map(str::to_string),
        })
    }

    /// Read one trace. Returns `None` when it does not exist **or** belongs to
    /// a key outside `scope`; callers must not tell the two apart.
    pub async fn get(
        &self,
        scope: &TraceScope,
        inference_id: Uuid,
    ) -> Result<Option<TraceDetail>, TraceStoreError> {
        let params = TraceListParams {
            limit: 1,
            window_hours: MAX_WINDOW_HOURS,
            before: None,
            function_name: None,
        };
        let Some(summary) = self
            .summaries(scope, &params, Some(inference_id))
            .await?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        let model_calls = self.model_calls(&summary).await?;
        let (input, output) = self.bodies(&summary).await?;
        let bodies_expired = input.is_none() && output.is_none();
        Ok(Some(TraceDetail {
            summary,
            model_calls,
            input,
            output,
            bodies_expired,
        }))
    }

    /// Push one `SELECT` branch over `chat_inferences` or `json_inferences`.
    /// `table` and `kind` are compile-time constants; every caller value is
    /// bound.
    fn push_branch(
        &self,
        builder: &mut QueryBuilder<Postgres>,
        branch: Branch,
        scope: &TraceScope,
        params: &TraceListParams,
        inference_id: Option<Uuid>,
    ) {
        let (table, kind) = match branch {
            Branch::Chat => ("chat_inferences", "'chat'"),
            Branch::Json => ("json_inferences", "'json'"),
        };
        builder.push(
            "SELECT id, function_name, variant_name, episode_id, processing_time_ms, ttft_ms, \
             tags->>",
        );
        builder.push_bind(API_KEY_TAG);
        builder.push(" AS api_key_public_id, created_at, ");
        builder.push(kind);
        builder.push("::text AS kind FROM ");
        builder.push(&self.schema);
        builder.push(".");
        builder.push(table);
        builder.push(" WHERE created_at >= NOW() - make_interval(hours => ");
        builder.push_bind(params.window_hours as i32);
        builder.push(")");
        if let Some(key) = scope.key_filter() {
            builder.push(" AND tags @> ");
            builder.push_bind(serde_json::json!({ API_KEY_TAG: key }));
        }
        if let Some(function_name) = &params.function_name {
            builder.push(" AND function_name = ");
            builder.push_bind(function_name.clone());
        }
        if let Some(cursor) = params.before {
            builder.push(" AND (created_at, id) < (to_timestamp(");
            builder.push_bind(cursor.created_at_us as f64 / 1_000_000.0);
            builder.push("), ");
            builder.push_bind(cursor.id);
            builder.push(")");
        }
        if let Some(inference_id) = inference_id {
            builder.push(" AND id = ");
            builder.push_bind(inference_id);
        }
        builder.push(" ORDER BY created_at DESC, id DESC LIMIT ");
        builder.push_bind(params.limit);
    }

    async fn summaries(
        &self,
        scope: &TraceScope,
        params: &TraceListParams,
        inference_id: Option<Uuid>,
    ) -> Result<Vec<TraceSummary>, TraceStoreError> {
        let mut builder = QueryBuilder::<Postgres>::new(
            "SELECT inf.id, inf.function_name, inf.variant_name, inf.episode_id, \
               inf.processing_time_ms, inf.ttft_ms, inf.api_key_public_id, inf.kind, \
               (extract(epoch FROM inf.created_at) * 1000000)::bigint AS created_at_us, \
               mi.model_name, mi.model_provider_name, mi.finish_reason, mi.input_tokens, \
               mi.output_tokens, mi.cost_micros, mi.model_calls, mi.cached \
             FROM ((",
        );
        self.push_branch(&mut builder, Branch::Chat, scope, params, inference_id);
        builder.push(") UNION ALL (");
        self.push_branch(&mut builder, Branch::Json, scope, params, inference_id);
        builder.push(
            ")) inf LEFT JOIN LATERAL ( \
               SELECT (array_agg(m.model_name ORDER BY m.created_at DESC, m.id DESC))[1] AS model_name, \
                 (array_agg(m.model_provider_name ORDER BY m.created_at DESC, m.id DESC))[1] \
                   AS model_provider_name, \
                 (array_agg(m.finish_reason ORDER BY m.created_at DESC, m.id DESC))[1] AS finish_reason, \
                 SUM(m.input_tokens)::bigint AS input_tokens, \
                 SUM(m.output_tokens)::bigint AS output_tokens, \
                 ROUND(SUM(m.cost) * 1000000)::bigint AS cost_micros, \
                 COUNT(*)::bigint AS model_calls, \
                 COALESCE(bool_and(m.cached), false) AS cached \
               FROM ",
        );
        builder.push(&self.schema);
        builder.push(
            ".model_inferences m \
               WHERE m.inference_id = inf.id \
                 AND m.created_at >= inf.created_at - INTERVAL '1 day' \
                 AND m.created_at <= inf.created_at + INTERVAL '1 day' \
             ) mi ON TRUE \
             ORDER BY inf.created_at DESC, inf.id DESC LIMIT ",
        );
        builder.push_bind(params.limit);
        let rows = builder.build().fetch_all(&self.pool).await?;
        rows.iter().map(summary_from_row).collect()
    }

    async fn model_calls(&self, summary: &TraceSummary) -> Result<Vec<ModelCall>, TraceStoreError> {
        let mut builder = QueryBuilder::<Postgres>::new(
            "SELECT id, model_name, model_provider_name, input_tokens, output_tokens, \
               response_time_ms, ttft_ms, ROUND(cost * 1000000)::bigint AS cost_micros, cached, \
               finish_reason, (extract(epoch FROM created_at) * 1000000)::bigint AS created_at_us \
             FROM ",
        );
        builder.push(&self.schema);
        builder.push(".model_inferences WHERE inference_id = ");
        builder.push_bind(summary.inference_id);
        push_day_window(&mut builder, summary.created_at_us);
        builder.push(" ORDER BY created_at, id");
        let rows = builder.build().fetch_all(&self.pool).await?;
        rows.iter()
            .map(|row| {
                Ok(ModelCall {
                    id: row.try_get("id")?,
                    model_name: row.try_get("model_name")?,
                    model_provider_name: row.try_get("model_provider_name")?,
                    input_tokens: row.try_get("input_tokens")?,
                    output_tokens: row.try_get("output_tokens")?,
                    response_time_ms: row.try_get("response_time_ms")?,
                    ttft_ms: row.try_get("ttft_ms")?,
                    cost_micros: row.try_get("cost_micros")?,
                    cached: row.try_get("cached")?,
                    finish_reason: row.try_get("finish_reason")?,
                    created_at_us: row.try_get("created_at_us")?,
                })
            })
            .collect()
    }

    async fn bodies(
        &self,
        summary: &TraceSummary,
    ) -> Result<(Option<serde_json::Value>, Option<serde_json::Value>), TraceStoreError> {
        let table = if summary.kind == "json" {
            "json_inference_data"
        } else {
            "chat_inference_data"
        };
        let mut builder = QueryBuilder::<Postgres>::new("SELECT input, output FROM ");
        builder.push(&self.schema);
        builder.push(".");
        builder.push(table);
        builder.push(" WHERE id = ");
        builder.push_bind(summary.inference_id);
        push_day_window(&mut builder, summary.created_at_us);
        builder.push(" LIMIT 1");
        let row = builder.build().fetch_optional(&self.pool).await?;
        match row {
            Some(row) => Ok((row.try_get("input")?, row.try_get("output")?)),
            None => Ok((None, None)),
        }
    }
}

/// Bound `created_at` to one day either side of the trace, so Postgres prunes
/// partitions on the body and model-call tables.
fn push_day_window(builder: &mut QueryBuilder<Postgres>, created_at_us: i64) {
    let seconds = created_at_us as f64 / 1_000_000.0;
    builder.push(" AND created_at >= to_timestamp(");
    builder.push_bind(seconds);
    builder.push(") - INTERVAL '1 day' AND created_at <= to_timestamp(");
    builder.push_bind(seconds);
    builder.push(") + INTERVAL '1 day'");
}

fn summary_from_row(row: &PgRow) -> Result<TraceSummary, TraceStoreError> {
    Ok(TraceSummary {
        inference_id: row.try_get("id")?,
        episode_id: row.try_get("episode_id")?,
        kind: row.try_get("kind")?,
        function_name: row.try_get("function_name")?,
        variant_name: row.try_get("variant_name")?,
        api_key_public_id: row.try_get("api_key_public_id")?,
        created_at_us: row.try_get("created_at_us")?,
        model_name: row.try_get("model_name")?,
        model_provider_name: row.try_get("model_provider_name")?,
        finish_reason: row.try_get("finish_reason")?,
        input_tokens: row.try_get("input_tokens")?,
        output_tokens: row.try_get("output_tokens")?,
        cost_micros: row.try_get("cost_micros")?,
        processing_time_ms: row.try_get("processing_time_ms")?,
        ttft_ms: row.try_get("ttft_ms")?,
        model_calls: row.try_get("model_calls")?,
        cached: row.try_get("cached")?,
    })
}
