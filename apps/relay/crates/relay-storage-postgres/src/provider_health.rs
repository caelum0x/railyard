//! Postgres checkpoints for Relay's adaptive provider scorer.

use relay_hedge::ProviderHealthSnapshot;
use sqlx::{PgPool, Row};

/// Durable store for restart-safe provider latency and circuit-breaker state.
#[derive(Clone)]
pub struct PostgresProviderHealthStore {
    pool: PgPool,
}

impl PostgresProviderHealthStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Load every valid checkpoint. An individually malformed row is ignored
    /// because adaptive ranking is an optimization, never billing authority.
    pub async fn load(&self) -> Result<Vec<ProviderHealthSnapshot>, sqlx::Error> {
        let rows = sqlx::query("SELECT snapshot FROM relay_provider_health")
            .fetch_all(&self.pool)
            .await?;
        let mut snapshots = Vec::with_capacity(rows.len());
        for row in rows {
            let value = row.try_get::<serde_json::Value, _>("snapshot")?;
            match serde_json::from_value(value) {
                Ok(snapshot) => snapshots.push(snapshot),
                Err(error) => {
                    tracing::warn!(
                        error = %error,
                        "Ignoring malformed Relay provider-health snapshot"
                    );
                }
            }
        }
        Ok(snapshots)
    }

    /// Atomically upsert one consistent snapshot of every observed provider.
    pub async fn persist(
        &self,
        snapshots: &[ProviderHealthSnapshot],
    ) -> Result<usize, sqlx::Error> {
        if snapshots.is_empty() {
            return Ok(0);
        }
        let mut transaction = self.pool.begin().await?;
        for snapshot in snapshots {
            let value = serde_json::to_value(snapshot)
                .map_err(|error| sqlx::Error::Encode(error.into()))?;
            sqlx::query(
                r"
                INSERT INTO relay_provider_health (model_name, provider_name, snapshot, updated_at)
                VALUES ($1, $2, $3, NOW())
                ON CONFLICT (model_name, provider_name) DO UPDATE
                SET snapshot = EXCLUDED.snapshot, updated_at = EXCLUDED.updated_at
                ",
            )
            .bind(&snapshot.model_name)
            .bind(&snapshot.provider_name)
            .bind(value)
            .execute(&mut *transaction)
            .await?;
        }
        transaction.commit().await?;
        Ok(snapshots.len())
    }
}
