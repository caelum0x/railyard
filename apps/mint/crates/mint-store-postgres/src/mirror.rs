//! Postgres [`MirrorStore`] (Wave P PY1.5): `mint_mirror_events` and its
//! delivery outbox (migration 0013). Runtime queries with `.bind` only.

use async_trait::async_trait;
use mint_core::MintError;
use mint_store::{MirrorDelivery, MirrorEvent, MirrorKind, MirrorStore};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};

/// The mirror on the production pool.
pub struct PgMirror {
    pool: PgPool,
}

impl PgMirror {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn storage(error: sqlx::Error) -> MintError {
    tracing::error!(%error, "mint-store-postgres: mirror query failed");
    MintError::Storage
}

const EVENT_COLS: &str = "e.id, e.product, e.provider, e.provider_event_id, e.kind, e.amount_micros, \
     e.currency, e.customer_ref, e.occurred_at_ms, e.recorded_at_ms";

fn event_from(row: &PgRow) -> Result<MirrorEvent, MintError> {
    let kind: String = row.try_get("kind").map_err(storage)?;
    Ok(MirrorEvent {
        id: row.try_get("id").map_err(storage)?,
        product: row.try_get("product").map_err(storage)?,
        provider: row.try_get("provider").map_err(storage)?,
        provider_event_id: row.try_get("provider_event_id").map_err(storage)?,
        kind: MirrorKind::parse(&kind).ok_or(MintError::Storage)?,
        amount_micros: row.try_get("amount_micros").map_err(storage)?,
        currency: row.try_get("currency").map_err(storage)?,
        customer_ref: row.try_get("customer_ref").map_err(storage)?,
        occurred_at_ms: row.try_get("occurred_at_ms").map_err(storage)?,
        recorded_at_ms: row.try_get("recorded_at_ms").map_err(storage)?,
    })
}

#[async_trait]
impl MirrorStore for PgMirror {
    async fn record(&self, event: MirrorEvent, endpoints: &[String]) -> Result<bool, MintError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let inserted = sqlx::query(
            "INSERT INTO mint_mirror_events (id, product, provider, provider_event_id, kind, amount_micros, \
               currency, customer_ref, occurred_at_ms, recorded_at_ms) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
             ON CONFLICT (provider, provider_event_id) DO NOTHING RETURNING id",
        )
        .bind(&event.id)
        .bind(&event.product)
        .bind(&event.provider)
        .bind(&event.provider_event_id)
        .bind(event.kind.as_str())
        .bind(event.amount_micros)
        .bind(&event.currency)
        .bind(&event.customer_ref)
        .bind(event.occurred_at_ms)
        .bind(event.recorded_at_ms)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?;
        if inserted.is_none() {
            tx.rollback().await.map_err(storage)?;
            return Ok(false);
        }
        for endpoint in endpoints {
            sqlx::query("INSERT INTO mint_mirror_deliveries (event_id, endpoint, next_attempt_ms) VALUES ($1, $2, $3)")
                .bind(&event.id)
                .bind(endpoint)
                .bind(event.recorded_at_ms)
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
        }
        tx.commit().await.map_err(storage)?;
        Ok(true)
    }

    async fn list(&self, product: &str, limit: usize) -> Result<Vec<MirrorEvent>, MintError> {
        let rows = sqlx::query(&format!(
            "SELECT {EVENT_COLS} FROM mint_mirror_events e WHERE e.product = $1 \
             ORDER BY e.recorded_at_ms DESC, e.id DESC LIMIT $2"
        ))
        .bind(product)
        .bind(i64::try_from(limit.min(1000)).unwrap_or(1000))
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter().map(event_from).collect()
    }

    async fn due(&self, now_ms: i64, limit: usize) -> Result<Vec<MirrorDelivery>, MintError> {
        let rows = sqlx::query(&format!(
            "SELECT {EVENT_COLS}, d.endpoint, d.attempts FROM mint_mirror_deliveries d \
             JOIN mint_mirror_events e ON e.id = d.event_id \
             WHERE d.delivered_ms IS NULL AND d.next_attempt_ms <= $1 \
             ORDER BY e.recorded_at_ms LIMIT $2"
        ))
        .bind(now_ms)
        .bind(i64::try_from(limit.min(1000)).unwrap_or(1000))
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?;
        rows.iter()
            .map(|row| {
                Ok(MirrorDelivery {
                    event: event_from(row)?,
                    endpoint: row.try_get("endpoint").map_err(storage)?,
                    attempts: row.try_get("attempts").map_err(storage)?,
                })
            })
            .collect()
    }

    async fn delivered(&self, event_id: &str, endpoint: &str, now_ms: i64) -> Result<(), MintError> {
        sqlx::query(
            "UPDATE mint_mirror_deliveries SET attempts = attempts + 1, delivered_ms = $3, last_error = NULL \
             WHERE event_id = $1 AND endpoint = $2",
        )
        .bind(event_id)
        .bind(endpoint)
        .bind(now_ms)
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        Ok(())
    }

    async fn failed(&self, event_id: &str, endpoint: &str, next_ms: i64, error: &str) -> Result<(), MintError> {
        sqlx::query(
            "UPDATE mint_mirror_deliveries SET attempts = attempts + 1, next_attempt_ms = $3, last_error = $4 \
             WHERE event_id = $1 AND endpoint = $2",
        )
        .bind(event_id)
        .bind(endpoint)
        .bind(next_ms)
        .bind(error.chars().take(300).collect::<String>())
        .execute(&self.pool)
        .await
        .map_err(storage)?;
        Ok(())
    }
}
