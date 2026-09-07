//! Durable settlement outbox: the crash-visible state machine that makes the
//! hold+capture+COGS trio atomic-or-compensated.
//!
//! Each request's settlement is one [`SettlementSaga`] with a durable [`SagaState`]
//! and per-step [`StepState`]s. The coordinator in [`crate::settle`] persists the
//! saga *before* touching Mint and after every step transition, so a crash at any
//! point leaves a recoverable record: a later attempt either resumes the forward
//! path (idempotent, via per-step keys) or drives compensation to completion.
//!
//! [`OutboxStore`] is the persistence port. [`MemoryOutbox`] keeps tests focused;
//! [`PostgresOutbox`] is wired in production so restart recovery is real.

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::{PgPool, PgRow};
use sqlx::Row;

use crate::domain::SettlementRefs;
use crate::error::LedgerlineError;

/// Lifecycle of one settlement step (hold, capture, or COGS transfer).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepState {
    /// Not yet attempted, or attempted-but-not-confirmed.
    Pending,
    /// Forward effect confirmed committed on Mint.
    Committed,
    /// Forward effect confirmed reversed on Mint.
    Compensated,
}

/// Lifecycle of the whole settlement saga for one request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SagaState {
    /// A forward attempt is in progress (or crashed mid-flight).
    Pending,
    /// All three steps committed. Terminal success.
    Settled,
    /// A forward step failed; committed steps are being reversed.
    Compensating,
    /// All committed steps have been reversed. No net money moved this attempt.
    Compensated,
}

/// Durable record of one request's settlement progress.
///
/// `attempt` is bumped on each fresh forward run after a compensated attempt, so
/// the coordinator can mint per-attempt idempotency keys. Reusing a compensated
/// attempt's keys would let Mint treat a fresh forward call as a no-op replay of
/// an already-reversed operation, so a new attempt gets new keys.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementSaga {
    pub request_id: String,
    /// Exact serialized financial input this saga is authorized to settle.
    pub event_json: String,
    pub attempt: u32,
    pub state: SagaState,
    pub hold_state: StepState,
    pub hold_id: Option<String>,
    pub hold_seq: Option<i64>,
    pub capture_state: StepState,
    pub revenue_seq: Option<i64>,
    pub cogs_state: StepState,
    pub cogs_seq: Option<i64>,
    pub updated_at: DateTime<Utc>,
}

impl SettlementSaga {
    /// A brand-new saga for a request, on its first forward attempt.
    pub fn new(
        request_id: impl Into<String>,
        event_json: impl Into<String>,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            request_id: request_id.into(),
            event_json: event_json.into(),
            attempt: 1,
            state: SagaState::Pending,
            hold_state: StepState::Pending,
            hold_id: None,
            hold_seq: None,
            capture_state: StepState::Pending,
            revenue_seq: None,
            cogs_state: StepState::Pending,
            cogs_seq: None,
            updated_at: now,
        }
    }

    /// Begin a fresh forward attempt after a fully compensated one. Step state is
    /// reset so the coordinator re-runs the trio with new per-attempt keys.
    pub fn next_attempt(&self, now: DateTime<Utc>) -> Self {
        Self {
            request_id: self.request_id.clone(),
            event_json: self.event_json.clone(),
            attempt: self.attempt.saturating_add(1),
            state: SagaState::Pending,
            hold_state: StepState::Pending,
            hold_id: None,
            hold_seq: None,
            capture_state: StepState::Pending,
            revenue_seq: None,
            cogs_state: StepState::Pending,
            cogs_seq: None,
            updated_at: now,
        }
    }

    /// The settlement references for a fully settled saga.
    pub fn refs(&self) -> SettlementRefs {
        SettlementRefs {
            revenue_hold_id: self.hold_id.clone(),
            revenue_sequence: self.revenue_seq,
            cogs_sequence: self.cogs_seq,
        }
    }
}

/// Persistence port for the settlement outbox.
#[async_trait]
pub trait OutboxStore: Send + Sync {
    /// Load the saga for a request, if one exists.
    async fn load(&self, request_id: &str) -> Result<Option<SettlementSaga>, LedgerlineError>;

    /// Create or replace the saga for a request. Idempotent by `request_id`.
    async fn upsert(&self, saga: &SettlementSaga) -> Result<(), LedgerlineError>;

    /// List incomplete sagas — those still `Pending` (crashed mid-forward) or
    /// `Compensating` (crashed mid-reversal) — that have not been touched since
    /// `older_than`, oldest first, up to `limit`.
    ///
    /// The `older_than` cutoff is what lets an autonomous recovery worker act
    /// only on genuinely stuck sagas without racing a live in-flight request
    /// (which updates `updated_at` on every step). The Postgres implementation
    /// serves this from the purpose-built partial recovery index on
    /// `(state, updated_at)`.
    async fn list_incomplete(
        &self,
        older_than: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<SettlementSaga>, LedgerlineError>;

    /// List `Settled` sagas whose request-level margin projection has not landed
    /// in `margin_events`, untouched since `older_than`, oldest first, up to
    /// `limit`.
    ///
    /// This closes the projection durability gap: money can be fully settled on
    /// Mint (saga `Settled`) yet the ingest process can crash before it writes
    /// the margin row, or the recovery worker can drive a crashed forward saga to
    /// `Settled` without ever projecting it. Neither case is a money bug — Mint
    /// stays correct — but the request's gross margin would be permanently
    /// invisible (`GET /v1/margins/:id` => 404, aggregates under-count) unless it
    /// is backfilled. The reconciliation sweep in [`crate::recovery`] consumes
    /// this list and rebuilds each missing projection from the saga's authorized
    /// event binding.
    ///
    /// The `older_than` cutoff keeps the sweep from racing a live request that
    /// just settled and is about to write its own projection.
    async fn list_unprojected_settled(
        &self,
        older_than: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<SettlementSaga>, LedgerlineError>;
}

/// In-process outbox for tests and single-node runs.
#[derive(Default)]
pub struct MemoryOutbox {
    sagas: RwLock<HashMap<String, SettlementSaga>>,
}

impl MemoryOutbox {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl OutboxStore for MemoryOutbox {
    async fn load(&self, request_id: &str) -> Result<Option<SettlementSaga>, LedgerlineError> {
        Ok(self
            .sagas
            .read()
            .map_err(|_| LedgerlineError::Storage("outbox lock poisoned".into()))?
            .get(request_id)
            .cloned())
    }

    async fn upsert(&self, saga: &SettlementSaga) -> Result<(), LedgerlineError> {
        self.sagas
            .write()
            .map_err(|_| LedgerlineError::Storage("outbox lock poisoned".into()))?
            .insert(saga.request_id.clone(), saga.clone());
        Ok(())
    }

    async fn list_incomplete(
        &self,
        older_than: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<SettlementSaga>, LedgerlineError> {
        let guard = self
            .sagas
            .read()
            .map_err(|_| LedgerlineError::Storage("outbox lock poisoned".into()))?;
        let mut stuck: Vec<SettlementSaga> = guard
            .values()
            .filter(|saga| {
                matches!(saga.state, SagaState::Pending | SagaState::Compensating)
                    && saga.updated_at < older_than
            })
            .cloned()
            .collect();
        stuck.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        if limit >= 0 {
            stuck.truncate(limit as usize);
        }
        Ok(stuck)
    }

    async fn list_unprojected_settled(
        &self,
        older_than: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<SettlementSaga>, LedgerlineError> {
        // The in-memory outbox cannot anti-join against the margin projection
        // store, so it returns every stale `Settled` saga; the reconciliation
        // sweep filters out those already projected via `EventStore`.
        let guard = self
            .sagas
            .read()
            .map_err(|_| LedgerlineError::Storage("outbox lock poisoned".into()))?;
        let mut settled: Vec<SettlementSaga> = guard
            .values()
            .filter(|saga| saga.state == SagaState::Settled && saga.updated_at < older_than)
            .cloned()
            .collect();
        settled.sort_by(|a, b| a.updated_at.cmp(&b.updated_at));
        if limit >= 0 {
            settled.truncate(limit as usize);
        }
        Ok(settled)
    }
}

/// Production outbox backed by the same Postgres database as margin events.
#[derive(Clone)]
pub struct PostgresOutbox {
    pool: PgPool,
}

impl PostgresOutbox {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl OutboxStore for PostgresOutbox {
    async fn load(&self, request_id: &str) -> Result<Option<SettlementSaga>, LedgerlineError> {
        let row = sqlx::query("SELECT * FROM settlement_outbox WHERE request_id = $1")
            .bind(request_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?;
        row.as_ref().map(row_to_saga).transpose()
    }

    async fn upsert(&self, saga: &SettlementSaga) -> Result<(), LedgerlineError> {
        sqlx::query(
            "INSERT INTO settlement_outbox (request_id, event_json, attempt, state, hold_state, hold_id, \
             hold_seq, capture_state, revenue_seq, cogs_state, cogs_seq, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) \
             ON CONFLICT (request_id) DO UPDATE SET \
             event_json = EXCLUDED.event_json, attempt = EXCLUDED.attempt, state = EXCLUDED.state, \
             hold_state = EXCLUDED.hold_state, hold_id = EXCLUDED.hold_id, \
             hold_seq = EXCLUDED.hold_seq, capture_state = EXCLUDED.capture_state, \
             revenue_seq = EXCLUDED.revenue_seq, cogs_state = EXCLUDED.cogs_state, \
             cogs_seq = EXCLUDED.cogs_seq, updated_at = EXCLUDED.updated_at",
        )
        .bind(&saga.request_id)
        .bind(&saga.event_json)
        .bind(i64::from(saga.attempt))
        .bind(saga_state_str(saga.state))
        .bind(step_state_str(saga.hold_state))
        .bind(&saga.hold_id)
        .bind(saga.hold_seq)
        .bind(step_state_str(saga.capture_state))
        .bind(saga.revenue_seq)
        .bind(step_state_str(saga.cogs_state))
        .bind(saga.cogs_seq)
        .bind(saga.updated_at)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(())
    }

    async fn list_incomplete(
        &self,
        older_than: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<SettlementSaga>, LedgerlineError> {
        // Served by the partial index `settlement_outbox_recovery` on
        // (state, updated_at) WHERE state IN ('pending','compensating').
        let rows = sqlx::query(
            "SELECT * FROM settlement_outbox \
             WHERE state IN ('pending','compensating') AND updated_at < $1 \
             ORDER BY updated_at ASC LIMIT $2",
        )
        .bind(older_than)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?;
        rows.iter().map(row_to_saga).collect()
    }

    async fn list_unprojected_settled(
        &self,
        older_than: DateTime<Utc>,
        limit: i64,
    ) -> Result<Vec<SettlementSaga>, LedgerlineError> {
        // Anti-join against the margin projection: a `Settled` saga with no
        // `margin_events` row is a money-visibility gap left by a crash between
        // settlement and the projection write. `margin_events.request_id` is
        // UNIQUE, so the NOT EXISTS probe is a single index lookup per row.
        let rows = sqlx::query(
            "SELECT o.* FROM settlement_outbox o \
             WHERE o.state = 'settled' AND o.updated_at < $1 \
               AND NOT EXISTS ( \
                   SELECT 1 FROM margin_events m WHERE m.request_id = o.request_id \
               ) \
             ORDER BY o.updated_at ASC LIMIT $2",
        )
        .bind(older_than)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?;
        rows.iter().map(row_to_saga).collect()
    }
}

fn storage_error(error: sqlx::Error) -> LedgerlineError {
    LedgerlineError::Storage(error.to_string())
}

fn saga_state_str(state: SagaState) -> &'static str {
    match state {
        SagaState::Pending => "pending",
        SagaState::Settled => "settled",
        SagaState::Compensating => "compensating",
        SagaState::Compensated => "compensated",
    }
}

fn parse_saga_state(value: &str) -> Result<SagaState, LedgerlineError> {
    match value {
        "pending" => Ok(SagaState::Pending),
        "settled" => Ok(SagaState::Settled),
        "compensating" => Ok(SagaState::Compensating),
        "compensated" => Ok(SagaState::Compensated),
        _ => Err(LedgerlineError::Storage(format!(
            "unknown settlement saga state {value}"
        ))),
    }
}

fn step_state_str(state: StepState) -> &'static str {
    match state {
        StepState::Pending => "pending",
        StepState::Committed => "committed",
        StepState::Compensated => "compensated",
    }
}

fn parse_step_state(value: &str) -> Result<StepState, LedgerlineError> {
    match value {
        "pending" => Ok(StepState::Pending),
        "committed" => Ok(StepState::Committed),
        "compensated" => Ok(StepState::Compensated),
        _ => Err(LedgerlineError::Storage(format!(
            "unknown settlement step state {value}"
        ))),
    }
}

fn row_to_saga(row: &PgRow) -> Result<SettlementSaga, LedgerlineError> {
    let attempt = row.try_get::<i64, _>("attempt").map_err(storage_error)?;
    let attempt = u32::try_from(attempt)
        .map_err(|_| LedgerlineError::Storage("outbox attempt is outside u32".into()))?;
    let state: String = row.try_get("state").map_err(storage_error)?;
    let hold_state: String = row.try_get("hold_state").map_err(storage_error)?;
    let capture_state: String = row.try_get("capture_state").map_err(storage_error)?;
    let cogs_state: String = row.try_get("cogs_state").map_err(storage_error)?;
    Ok(SettlementSaga {
        request_id: row.try_get("request_id").map_err(storage_error)?,
        event_json: row.try_get("event_json").map_err(storage_error)?,
        attempt,
        state: parse_saga_state(&state)?,
        hold_state: parse_step_state(&hold_state)?,
        hold_id: row.try_get("hold_id").map_err(storage_error)?,
        hold_seq: row.try_get("hold_seq").map_err(storage_error)?,
        capture_state: parse_step_state(&capture_state)?,
        revenue_seq: row.try_get("revenue_seq").map_err(storage_error)?,
        cogs_state: parse_step_state(&cogs_state)?,
        cogs_seq: row.try_get("cogs_seq").map_err(storage_error)?,
        updated_at: row.try_get("updated_at").map_err(storage_error)?,
    })
}

#[cfg(test)]
mod postgres_mapping_tests {
    use super::*;

    #[test]
    fn database_state_values_round_trip() {
        for state in [
            SagaState::Pending,
            SagaState::Settled,
            SagaState::Compensating,
            SagaState::Compensated,
        ] {
            assert_eq!(parse_saga_state(saga_state_str(state)).unwrap(), state);
        }
        for state in [
            StepState::Pending,
            StepState::Committed,
            StepState::Compensated,
        ] {
            assert_eq!(parse_step_state(step_state_str(state)).unwrap(), state);
        }
    }
}
