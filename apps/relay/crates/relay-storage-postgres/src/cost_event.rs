//! Postgres implementation of Relay's durable cost-event store.

use async_trait::async_trait;
use relay_cost_feed::{
    CostEvent, CostEventListQuery, CostEventPage, CostEventStore, CostFeedError, PendingCostEvent,
    Result,
};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Clone)]
pub struct PostgresCostEventStore {
    pool: PgPool,
}

impl PostgresCostEventStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl CostEventStore for PostgresCostEventStore {
    async fn append(&self, event: CostEvent) -> Result<()> {
        event
            .validate()
            .map_err(|error| CostFeedError::InvalidEvent(error.to_string()))?;
        let event_json = serde_json::to_value(&event)
            .map_err(|error| CostFeedError::Store(error.to_string()))?;
        let result = sqlx::query(
            "INSERT INTO relay_cost_events \
             (event_id,request_id,owner_principal,event,next_attempt_at) \
             VALUES ($1,$2,$3,$4,to_timestamp($5::double precision / 1000.0)) \
             ON CONFLICT (event_id) DO UPDATE SET event=EXCLUDED.event \
             WHERE relay_cost_events.event=EXCLUDED.event",
        )
        .bind(event.event_id)
        .bind(event.request_id)
        .bind(&event.owner_principal)
        .bind(event_json)
        .bind(event.settled_at_ms as f64)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        if result.rows_affected() != 1 {
            return Err(CostFeedError::Conflict);
        }
        Ok(())
    }

    async fn get(&self, owner_principal: &str, event_id: Uuid) -> Result<Option<PendingCostEvent>> {
        self.get_scoped(Some(owner_principal), event_id).await
    }

    async fn list(
        &self,
        owner_principal: &str,
        query: CostEventListQuery,
    ) -> Result<CostEventPage> {
        self.list_scoped(Some(owner_principal), query).await
    }

    async fn replay_dead_letter(
        &self,
        owner_principal: &str,
        event_id: Uuid,
        now_ms: i64,
    ) -> Result<PendingCostEvent> {
        self.replay_scoped(Some(owner_principal), event_id, now_ms)
            .await
    }

    async fn list_all(&self, query: CostEventListQuery) -> Result<CostEventPage> {
        self.list_scoped(None, query).await
    }

    async fn get_any(&self, event_id: Uuid) -> Result<Option<PendingCostEvent>> {
        self.get_scoped(None, event_id).await
    }

    async fn replay_dead_letter_any(
        &self,
        event_id: Uuid,
        now_ms: i64,
    ) -> Result<PendingCostEvent> {
        self.replay_scoped(None, event_id, now_ms).await
    }

    async fn claim(
        &self,
        worker_id: &str,
        now_ms: i64,
        lease_ms: i64,
        limit: usize,
    ) -> Result<Vec<PendingCostEvent>> {
        let limit = i64::try_from(limit).map_err(|_| CostFeedError::Overflow)?;
        let rows = sqlx::query(
            "WITH due AS (SELECT event_id FROM relay_cost_events \
             WHERE published_at IS NULL AND dead_lettered_at IS NULL \
             AND next_attempt_at <= to_timestamp($1::double precision / 1000.0) \
             AND (lease_until IS NULL OR lease_until <= NOW()) \
             ORDER BY next_attempt_at,event_id FOR UPDATE SKIP LOCKED LIMIT $2) \
             UPDATE relay_cost_events e SET lease_owner=$3, \
             lease_until=NOW()+make_interval(secs => $4), \
             attempt_count=attempt_count+1,updated_at=NOW() FROM due \
             WHERE e.event_id=due.event_id RETURNING e.event,e.attempt_count, \
             (extract(epoch FROM e.next_attempt_at)*1000)::bigint AS next_attempt_at_ms, \
             e.lease_owner,(extract(epoch FROM e.lease_until)*1000)::bigint AS lease_expires_at_ms, \
             (extract(epoch FROM e.published_at)*1000)::bigint AS published_at_ms, \
             (extract(epoch FROM e.dead_lettered_at)*1000)::bigint AS dead_lettered_at_ms,e.last_error",
        )
        .bind(now_ms as f64)
        .bind(limit)
        .bind(worker_id)
        .bind(lease_ms as f64 / 1_000.0)
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?;
        rows.iter().map(decode_pending).collect()
    }

    async fn mark_published(
        &self,
        event_id: Uuid,
        worker_id: &str,
        published_at_ms: i64,
    ) -> Result<()> {
        let result = sqlx::query(
            "UPDATE relay_cost_events SET published_at=to_timestamp($3::double precision / 1000.0), \
             lease_owner=NULL,lease_until=NULL,last_error=NULL,updated_at=NOW() \
             WHERE event_id=$1 AND lease_owner=$2 AND published_at IS NULL",
        )
        .bind(event_id)
        .bind(worker_id)
        .bind(published_at_ms as f64)
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        require_one(result.rows_affected())
    }

    async fn reschedule(
        &self,
        event_id: Uuid,
        worker_id: &str,
        next_attempt_at_ms: i64,
        error: &str,
    ) -> Result<()> {
        let result = sqlx::query(
            "UPDATE relay_cost_events SET next_attempt_at=to_timestamp($3::double precision / 1000.0), \
             last_error=$4,lease_owner=NULL,lease_until=NULL,updated_at=NOW() \
             WHERE event_id=$1 AND lease_owner=$2 AND published_at IS NULL AND dead_lettered_at IS NULL",
        )
        .bind(event_id)
        .bind(worker_id)
        .bind(next_attempt_at_ms as f64)
        .bind(truncate_error(error))
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        require_one(result.rows_affected())
    }

    async fn dead_letter(
        &self,
        event_id: Uuid,
        worker_id: &str,
        now_ms: i64,
        error: &str,
    ) -> Result<()> {
        let result = sqlx::query(
            "UPDATE relay_cost_events SET dead_lettered_at=to_timestamp($3::double precision / 1000.0), \
             last_error=$4,lease_owner=NULL,lease_until=NULL,updated_at=NOW() \
             WHERE event_id=$1 AND lease_owner=$2 AND published_at IS NULL AND dead_lettered_at IS NULL",
        )
        .bind(event_id)
        .bind(worker_id)
        .bind(now_ms as f64)
        .bind(truncate_error(error))
        .execute(&self.pool)
        .await
        .map_err(db_error)?;
        require_one(result.rows_affected())
    }
}

impl PostgresCostEventStore {
    /// Look up one event. `None` is the operator scope (every principal).
    async fn get_scoped(
        &self,
        owner_principal: Option<&str>,
        event_id: Uuid,
    ) -> Result<Option<PendingCostEvent>> {
        let row = sqlx::query(
            "SELECT event,attempt_count, \
             (extract(epoch FROM next_attempt_at)*1000)::bigint AS next_attempt_at_ms, \
             lease_owner,(extract(epoch FROM lease_until)*1000)::bigint AS lease_expires_at_ms, \
             (extract(epoch FROM published_at)*1000)::bigint AS published_at_ms, \
             (extract(epoch FROM dead_lettered_at)*1000)::bigint AS dead_lettered_at_ms,last_error \
             FROM relay_cost_events WHERE event_id=$1 AND ($2::text IS NULL OR owner_principal=$2)",
        )
        .bind(event_id)
        .bind(owner_principal)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?;
        row.as_ref().map(decode_pending).transpose()
    }

    /// List events newest-first. `None` is the operator scope.
    async fn list_scoped(
        &self,
        owner_principal: Option<&str>,
        query: CostEventListQuery,
    ) -> Result<CostEventPage> {
        let query = query.validate()?;
        let limit = i64::try_from(query.limit).map_err(|_| CostFeedError::Overflow)?;
        let state = query.state.map(|state| state.as_str());
        let rows = sqlx::query(
            "SELECT event,attempt_count, \
             (extract(epoch FROM next_attempt_at)*1000)::bigint AS next_attempt_at_ms, \
             lease_owner,(extract(epoch FROM lease_until)*1000)::bigint AS lease_expires_at_ms, \
             (extract(epoch FROM published_at)*1000)::bigint AS published_at_ms, \
             (extract(epoch FROM dead_lettered_at)*1000)::bigint AS dead_lettered_at_ms,last_error \
             FROM relay_cost_events WHERE ($1::text IS NULL OR owner_principal=$1) \
             AND ($2::uuid IS NULL OR event_id < $2) \
             AND ($3::text IS NULL \
               OR ($3='pending' AND published_at IS NULL AND dead_lettered_at IS NULL) \
               OR ($3='published' AND published_at IS NOT NULL) \
               OR ($3='dead_lettered' AND dead_lettered_at IS NOT NULL)) \
             ORDER BY event_id DESC LIMIT $4",
        )
        .bind(owner_principal)
        .bind(query.before)
        .bind(state)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(db_error)?;
        let items = rows
            .iter()
            .map(decode_pending)
            .collect::<Result<Vec<_>>>()?;
        let next_cursor = if items.len() == query.limit {
            items.last().map(|pending| pending.event.event_id)
        } else {
            None
        };
        Ok(CostEventPage { items, next_cursor })
    }

    /// Re-arm a dead letter for delivery. `None` is the operator scope.
    async fn replay_scoped(
        &self,
        owner_principal: Option<&str>,
        event_id: Uuid,
        now_ms: i64,
    ) -> Result<PendingCostEvent> {
        let row = sqlx::query(
            "UPDATE relay_cost_events SET attempt_count=0, \
             next_attempt_at=to_timestamp($3::double precision / 1000.0), \
             lease_owner=NULL,lease_until=NULL,dead_lettered_at=NULL,last_error=NULL,updated_at=NOW() \
             WHERE event_id=$1 AND ($2::text IS NULL OR owner_principal=$2) AND published_at IS NULL \
             AND dead_lettered_at IS NOT NULL \
             RETURNING event,attempt_count, \
             (extract(epoch FROM next_attempt_at)*1000)::bigint AS next_attempt_at_ms, \
             lease_owner,(extract(epoch FROM lease_until)*1000)::bigint AS lease_expires_at_ms, \
             (extract(epoch FROM published_at)*1000)::bigint AS published_at_ms, \
             (extract(epoch FROM dead_lettered_at)*1000)::bigint AS dead_lettered_at_ms,last_error",
        )
        .bind(event_id)
        .bind(owner_principal)
        .bind(now_ms)
        .fetch_optional(&self.pool)
        .await
        .map_err(db_error)?;
        if let Some(row) = row.as_ref() {
            return decode_pending(row);
        }
        if self.get_scoped(owner_principal, event_id).await?.is_some() {
            Err(CostFeedError::NotReplayable)
        } else {
            Err(CostFeedError::NotFound)
        }
    }
}

fn decode_pending(row: &sqlx::postgres::PgRow) -> Result<PendingCostEvent> {
    let event = serde_json::from_value(row.try_get("event").map_err(db_error)?)
        .map_err(|error| CostFeedError::Store(error.to_string()))?;
    Ok(PendingCostEvent {
        event,
        attempts: row
            .try_get::<i32, _>("attempt_count")
            .map_err(db_error)?
            .try_into()
            .map_err(|_| CostFeedError::Store("negative attempt count".into()))?,
        next_attempt_at_ms: row.try_get("next_attempt_at_ms").map_err(db_error)?,
        lease_owner: row.try_get("lease_owner").map_err(db_error)?,
        lease_expires_at_ms: row.try_get("lease_expires_at_ms").map_err(db_error)?,
        published_at_ms: row.try_get("published_at_ms").map_err(db_error)?,
        dead_lettered_at_ms: row.try_get("dead_lettered_at_ms").map_err(db_error)?,
        last_error: row.try_get("last_error").map_err(db_error)?,
    })
}

fn require_one(rows: u64) -> Result<()> {
    if rows != 1 {
        return Err(CostFeedError::LeaseOwnership);
    }
    Ok(())
}

fn truncate_error(error: &str) -> String {
    error.chars().take(1_024).collect()
}

fn db_error(error: impl std::fmt::Display) -> CostFeedError {
    CostFeedError::Store(error.to_string())
}
