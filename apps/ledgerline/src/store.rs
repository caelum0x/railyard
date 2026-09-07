//! Persistence ports plus the production Postgres implementation.

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::Row;

use crate::domain::{
    EventOutcome, EventSource, MarginFilter, MarginRecord, MeterEvent, SettlementRefs,
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
    async fn ready(&self) -> Result<(), LedgerlineError>;
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
            })
            .cloned()
            .collect();
        result.sort_by_key(|record| std::cmp::Reverse(record.created_at));
        result.truncate(limit);
        Ok(result)
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
             ORDER BY created_at DESC LIMIT $4",
        )
        .bind(&filter.customer_id)
        .bind(&filter.feature)
        .bind(&filter.model)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?;
        rows.iter().map(row_to_record).collect()
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
