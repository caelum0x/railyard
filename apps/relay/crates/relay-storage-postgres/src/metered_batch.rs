//! Ownership of postpaid (meter-only) provider batches.
//!
//! A batch starts synchronously but completes asynchronously. Meter-only mode
//! records the owning API key and retail selector at start, then records the
//! completed batch's usage exactly once when it is first observed complete.

use sqlx::{PgPool, Row};
use uuid::Uuid;

/// A meter-only batch's durable ownership record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeteredBatch {
    pub batch_id: Uuid,
    pub owner_principal: String,
    pub model: String,
    pub metered: bool,
}

#[derive(Clone)]
pub struct PostgresMeteredBatchStore {
    pool: PgPool,
}

impl PostgresMeteredBatchStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Persist ownership for a started batch. Re-recording the same batch is a
    /// no-op so a retried start never reassigns the owner.
    pub async fn record_start(
        &self,
        batch_id: Uuid,
        owner_principal: &str,
        model: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO relay_metered_batches (batch_id, owner_principal, model) \
             VALUES ($1, $2, $3) ON CONFLICT (batch_id) DO NOTHING",
        )
        .bind(batch_id)
        .bind(owner_principal)
        .bind(model)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get(&self, batch_id: Uuid) -> Result<Option<MeteredBatch>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT batch_id, owner_principal, model, metered_at IS NOT NULL AS metered \
             FROM relay_metered_batches WHERE batch_id = $1",
        )
        .bind(batch_id)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(MeteredBatch {
                batch_id: row.try_get("batch_id")?,
                owner_principal: row.try_get("owner_principal")?,
                model: row.try_get("model")?,
                metered: row.try_get("metered")?,
            })
        })
        .transpose()
    }

    /// Mark a completed batch as metered. Returns `false` when another poll
    /// already did so; per-inference cost events stay idempotent regardless.
    pub async fn mark_metered(&self, batch_id: Uuid) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            "UPDATE relay_metered_batches SET metered_at = NOW() \
             WHERE batch_id = $1 AND metered_at IS NULL",
        )
        .bind(batch_id)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }
}
