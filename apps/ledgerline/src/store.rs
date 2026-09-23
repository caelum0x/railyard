//! Persistence ports plus the production Postgres implementation.

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::Row;

use crate::domain::{
    EventOutcome, EventSource, GroupBy, MarginFilter, MarginRecord, MarginSummaryQuery,
    MarginSummaryRow, MeterEvent, SettlementRefs,
};
use crate::error::LedgerlineError;

#[async_trait]
pub trait EventStore: Send + Sync {
    /// Persist a record and report whether this call created it. Implementations
    /// must derive `created` from the atomic insert result, not timestamp equality.
    async fn save(&self, record: &MarginRecord) -> Result<(MarginRecord, bool), LedgerlineError>;
    async fn by_idempotency_key(&self, key: &str) -> Result<Option<MarginRecord>, LedgerlineError>;
    async fn by_request_id(
        &self,
        request_id: &str,
    ) -> Result<Option<MarginRecord>, LedgerlineError>;
    async fn list(&self, filter: &MarginFilter) -> Result<Vec<MarginRecord>, LedgerlineError>;
    /// Aggregate margin records into rollup buckets (largest revenue first).
    async fn summarize(
        &self,
        query: &MarginSummaryQuery,
    ) -> Result<Vec<MarginSummaryRow>, LedgerlineError>;
    async fn ready(&self) -> Result<(), LedgerlineError>;
}

/// The bucket key of a record for a rollup dimension.
fn group_key(record: &MarginRecord, group_by: GroupBy) -> String {
    match group_by {
        GroupBy::Customer => record.event.customer_id.clone(),
        GroupBy::Feature => record.event.feature.clone(),
        GroupBy::Model => record.event.model.clone(),
        GroupBy::Day => record.created_at.format("%Y-%m-%d").to_string(),
    }
}

/// Lightweight store for focused server tests and embedded demos.
#[derive(Default)]
pub struct MemoryStore {
    records: RwLock<HashMap<String, MarginRecord>>,
}

#[async_trait]
impl EventStore for MemoryStore {
    async fn save(&self, record: &MarginRecord) -> Result<(MarginRecord, bool), LedgerlineError> {
        let mut records = self
            .records
            .write()
            .map_err(|_| LedgerlineError::Storage("memory store lock poisoned".into()))?;
        if let Some(existing) = records.get(&record.event.idempotency_key) {
            return Ok((existing.clone(), false));
        }
        if records
            .values()
            .any(|existing| existing.event.request_id == record.event.request_id)
        {
            return Err(LedgerlineError::IdempotencyConflict);
        }
        records.insert(record.event.idempotency_key.clone(), record.clone());
        Ok((record.clone(), true))
    }

    async fn by_idempotency_key(&self, key: &str) -> Result<Option<MarginRecord>, LedgerlineError> {
        Ok(self
            .records
            .read()
            .map_err(|_| LedgerlineError::Storage("memory store lock poisoned".into()))?
            .get(key)
            .cloned())
    }

    async fn by_request_id(
        &self,
        request_id: &str,
    ) -> Result<Option<MarginRecord>, LedgerlineError> {
        Ok(self
            .records
            .read()
            .map_err(|_| LedgerlineError::Storage("memory store lock poisoned".into()))?
            .values()
            .find(|record| record.event.request_id == request_id)
            .cloned())
    }

    async fn list(&self, filter: &MarginFilter) -> Result<Vec<MarginRecord>, LedgerlineError> {
        let limit = filter.limit.unwrap_or(100).clamp(1, 1_000) as usize;
        let mut result: Vec<_> = self
            .records
            .read()
            .map_err(|_| LedgerlineError::Storage("memory store lock poisoned".into()))?
            .values()
            .filter(|record| {
                filter
                    .customer_id
                    .as_ref()
                    .is_none_or(|value| &record.event.customer_id == value)
                    && filter
                        .feature
                        .as_ref()
                        .is_none_or(|value| &record.event.feature == value)
                    && filter
                        .model
                        .as_ref()
                        .is_none_or(|value| &record.event.model == value)
                    && filter.from.is_none_or(|from| record.created_at >= from)
                    && filter.to.is_none_or(|to| record.created_at < to)
                    && filter.before.is_none_or(|before| record.created_at < before)
            })
            .cloned()
            .collect();
        result.sort_by_key(|record| std::cmp::Reverse(record.created_at));
        result.truncate(limit);
        Ok(result)
    }

    async fn summarize(
        &self,
        query: &MarginSummaryQuery,
    ) -> Result<Vec<MarginSummaryRow>, LedgerlineError> {
        let records = self
            .records
            .read()
            .map_err(|_| LedgerlineError::Storage("memory store lock poisoned".into()))?;
        let mut buckets: HashMap<String, MarginSummaryRow> = HashMap::new();
        for record in records.values().filter(|record| {
            query
                .customer_id
                .as_ref()
                .is_none_or(|v| &record.event.customer_id == v)
                && query.feature.as_ref().is_none_or(|v| &record.event.feature == v)
                && query.model.as_ref().is_none_or(|v| &record.event.model == v)
                && query.from.is_none_or(|from| record.created_at >= from)
                && query.to.is_none_or(|to| record.created_at < to)
        }) {
            let key = group_key(record, query.group_by);
            buckets
                .entry(key.clone())
                .or_insert_with(|| MarginSummaryRow {
                    key,
                    ..MarginSummaryRow::default()
                })
                .add(record);
        }
        let mut rows: Vec<MarginSummaryRow> =
            buckets.into_values().map(MarginSummaryRow::finish).collect();
        rows.sort_by(|a, b| b.revenue_micros.cmp(&a.revenue_micros).then(a.key.cmp(&b.key)));
        Ok(rows)
    }

    async fn ready(&self) -> Result<(), LedgerlineError> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct PostgresStore {
    pool: PgPool,
}

impl PostgresStore {
    pub async fn connect(database_url: &str) -> Result<Self, LedgerlineError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await
            .map_err(storage_error)?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> Result<(), LedgerlineError> {
        sqlx::migrate!()
            .run(&self.pool)
            .await
            .map_err(|error| LedgerlineError::Storage(error.to_string()))
    }

    /// Cloneable pool handle shared with the durable settlement outbox.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }
}

#[async_trait]
impl EventStore for PostgresStore {
    async fn save(&self, record: &MarginRecord) -> Result<(MarginRecord, bool), LedgerlineError> {
        let source_receipt = record
            .event
            .source_settlement_receipt
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .map_err(|error| LedgerlineError::Storage(error.to_string()))?;
        let result = sqlx::query(
            "INSERT INTO margin_events (idempotency_key, request_id, customer_id, feature, model, \
             source, outcome, revenue_micros, cogs_micros, margin_micros, margin_bps, \
             customer_account, revenue_account, cogs_account, provider_account, \
             source_settlement_receipt, revenue_hold_id, revenue_sequence, cogs_sequence, created_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20) \
             ON CONFLICT (idempotency_key) DO NOTHING",
        )
        .bind(&record.event.idempotency_key)
        .bind(&record.event.request_id)
        .bind(&record.event.customer_id)
        .bind(&record.event.feature)
        .bind(&record.event.model)
        .bind(source_str(record.event.source))
        .bind(outcome_str(record.event.outcome))
        .bind(record.event.revenue_micros)
        .bind(record.event.cogs_micros)
        .bind(record.margin_micros)
        .bind(record.margin_bps)
        .bind(&record.event.customer_account)
        .bind(&record.event.revenue_account)
        .bind(&record.event.cogs_account)
        .bind(&record.event.provider_account)
        .bind(source_receipt)
        .bind(&record.settlement.revenue_hold_id)
        .bind(record.settlement.revenue_sequence)
        .bind(record.settlement.cogs_sequence)
        .bind(record.created_at)
        .execute(&self.pool)
        .await;

        match result {
            Ok(result) => {
                let stored = self
                    .by_idempotency_key(&record.event.idempotency_key)
                    .await?
                    .ok_or_else(|| LedgerlineError::Storage("saved event disappeared".into()))?;
                Ok((stored, result.rows_affected() == 1))
            }
            Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
                Err(LedgerlineError::IdempotencyConflict)
            }
            Err(error) => Err(storage_error(error)),
        }
    }

    async fn by_idempotency_key(&self, key: &str) -> Result<Option<MarginRecord>, LedgerlineError> {
        let row = sqlx::query("SELECT * FROM margin_events WHERE idempotency_key = $1")
            .bind(key)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?;
        row.as_ref().map(row_to_record).transpose()
    }

    async fn by_request_id(
        &self,
        request_id: &str,
    ) -> Result<Option<MarginRecord>, LedgerlineError> {
        let row = sqlx::query("SELECT * FROM margin_events WHERE request_id = $1")
            .bind(request_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?;
        row.as_ref().map(row_to_record).transpose()
    }

    async fn list(&self, filter: &MarginFilter) -> Result<Vec<MarginRecord>, LedgerlineError> {
        let limit = filter.limit.unwrap_or(100).clamp(1, 1_000);
        let rows = sqlx::query(
            "SELECT * FROM margin_events \
             WHERE ($1::TEXT IS NULL OR customer_id = $1) \
               AND ($2::TEXT IS NULL OR feature = $2) \
               AND ($3::TEXT IS NULL OR model = $3) \
               AND ($5::TIMESTAMPTZ IS NULL OR created_at >= $5) \
               AND ($6::TIMESTAMPTZ IS NULL OR created_at < $6) \
               AND ($7::TIMESTAMPTZ IS NULL OR created_at < $7) \
             ORDER BY created_at DESC LIMIT $4",
        )
        .bind(&filter.customer_id)
        .bind(&filter.feature)
        .bind(&filter.model)
        .bind(limit)
        .bind(filter.from)
        .bind(filter.to)
        .bind(filter.before)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?;
        rows.iter().map(row_to_record).collect()
    }

    async fn summarize(
        &self,
        query: &MarginSummaryQuery,
    ) -> Result<Vec<MarginSummaryRow>, LedgerlineError> {
        // The grouping column comes from a closed enum, never from input text.
        let key_expr = match query.group_by {
            GroupBy::Customer => "customer_id",
            GroupBy::Feature => "feature",
            GroupBy::Model => "model",
            GroupBy::Day => "to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD')",
        };
        let sql = format!(
            "SELECT {key_expr} AS key, \
                    count(*)::BIGINT AS requests, \
                    count(*) FILTER (WHERE outcome = 'failed')::BIGINT AS failed_requests, \
                    count(*) FILTER (WHERE margin_micros < 0)::BIGINT AS negative_margin_requests, \
                    COALESCE(sum(revenue_micros), 0)::BIGINT AS revenue_micros, \
                    COALESCE(sum(cogs_micros), 0)::BIGINT AS cogs_micros, \
                    COALESCE(sum(margin_micros), 0)::BIGINT AS margin_micros \
             FROM margin_events \
             WHERE ($1::TEXT IS NULL OR customer_id = $1) \
               AND ($2::TEXT IS NULL OR feature = $2) \
               AND ($3::TEXT IS NULL OR model = $3) \
               AND ($4::TIMESTAMPTZ IS NULL OR created_at >= $4) \
               AND ($5::TIMESTAMPTZ IS NULL OR created_at < $5) \
             GROUP BY 1 ORDER BY revenue_micros DESC, key ASC LIMIT 1000"
        );
        let rows = sqlx::query(&sql)
            .bind(&query.customer_id)
            .bind(&query.feature)
            .bind(&query.model)
            .bind(query.from)
            .bind(query.to)
            .fetch_all(&self.pool)
            .await
            .map_err(storage_error)?;
        rows.iter()
            .map(|row| {
                Ok(MarginSummaryRow {
                    key: row.try_get("key").map_err(storage_error)?,
                    requests: row.try_get("requests").map_err(storage_error)?,
                    failed_requests: row.try_get("failed_requests").map_err(storage_error)?,
                    negative_margin_requests: row
                        .try_get("negative_margin_requests")
                        .map_err(storage_error)?,
                    revenue_micros: row.try_get("revenue_micros").map_err(storage_error)?,
                    cogs_micros: row.try_get("cogs_micros").map_err(storage_error)?,
                    margin_micros: row.try_get("margin_micros").map_err(storage_error)?,
                    margin_bps: None,
                }
                .finish())
            })
            .collect()
    }

    async fn ready(&self) -> Result<(), LedgerlineError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(storage_error)?;
        Ok(())
    }
}

fn storage_error(error: sqlx::Error) -> LedgerlineError {
    LedgerlineError::Storage(error.to_string())
}

fn source_str(source: EventSource) -> &'static str {
    match source {
        EventSource::RelayGateway => "relay_gateway",
        EventSource::Sdk => "sdk",
        EventSource::Reelforge => "reelforge",
    }
}

fn parse_source(value: &str) -> Result<EventSource, LedgerlineError> {
    match value {
        "relay_gateway" => Ok(EventSource::RelayGateway),
        "sdk" => Ok(EventSource::Sdk),
        "reelforge" => Ok(EventSource::Reelforge),
        _ => Err(LedgerlineError::Storage(format!(
            "unknown event source {value}"
        ))),
    }
}

fn outcome_str(outcome: EventOutcome) -> &'static str {
    match outcome {
        EventOutcome::Succeeded => "succeeded",
        EventOutcome::Failed => "failed",
    }
}

fn parse_outcome(value: &str) -> Result<EventOutcome, LedgerlineError> {
    match value {
        "succeeded" => Ok(EventOutcome::Succeeded),
        "failed" => Ok(EventOutcome::Failed),
        _ => Err(LedgerlineError::Storage(format!(
            "unknown event outcome {value}"
        ))),
    }
}

fn row_to_record(row: &PgRow) -> Result<MarginRecord, LedgerlineError> {
    let source: String = row.try_get("source").map_err(storage_error)?;
    let outcome: String = row.try_get("outcome").map_err(storage_error)?;
    let source_receipt: Option<String> = row
        .try_get("source_settlement_receipt")
        .map_err(storage_error)?;
    let source_settlement_receipt = source_receipt
        .map(|value| serde_json::from_str(&value))
        .transpose()
        .map_err(|error| LedgerlineError::Storage(error.to_string()))?;
    Ok(MarginRecord {
        event: MeterEvent {
            idempotency_key: row.try_get("idempotency_key").map_err(storage_error)?,
            request_id: row.try_get("request_id").map_err(storage_error)?,
            customer_id: row.try_get("customer_id").map_err(storage_error)?,
            feature: row.try_get("feature").map_err(storage_error)?,
            model: row.try_get("model").map_err(storage_error)?,
            source: parse_source(&source)?,
            outcome: parse_outcome(&outcome)?,
            revenue_micros: row.try_get("revenue_micros").map_err(storage_error)?,
            cogs_micros: row.try_get("cogs_micros").map_err(storage_error)?,
            customer_account: row.try_get("customer_account").map_err(storage_error)?,
            revenue_account: row.try_get("revenue_account").map_err(storage_error)?,
            cogs_account: row.try_get("cogs_account").map_err(storage_error)?,
            provider_account: row.try_get("provider_account").map_err(storage_error)?,
            source_settlement_receipt,
        },
        margin_micros: row.try_get("margin_micros").map_err(storage_error)?,
        margin_bps: row.try_get("margin_bps").map_err(storage_error)?,
        settlement: SettlementRefs {
            revenue_hold_id: row.try_get("revenue_hold_id").map_err(storage_error)?,
            revenue_sequence: row.try_get("revenue_sequence").map_err(storage_error)?,
            cogs_sequence: row.try_get("cogs_sequence").map_err(storage_error)?,
        },
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(storage_error)?,
        idempotent_replay: false,
    })
}
