//! Shared Postgres recovery state for horizontally-scaled Abacus replicas.

use abacus_core::error::AbacusError;
use abacus_core::hold_store::{HoldRecord, HoldStore};
use abacus_core::receipt::{MintReceipt, ReceiptStore};
use async_trait::async_trait;
use sqlx::postgres::{PgPool, PgPoolOptions};
use sqlx::Row;

#[derive(Clone)]
pub struct PostgresHoldStore {
    pool: PgPool,
}

impl PostgresHoldStore {
    pub async fn connect(database_url: &str) -> Result<Self, AbacusError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await
            .map_err(storage)?;
        Ok(Self { pool })
    }

    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn migrate(&self) -> Result<(), AbacusError> {
        sqlx::migrate!()
            .run(&self.pool)
            .await
            .map_err(|error| AbacusError::Storage(error.to_string()))
    }

    fn row(record: sqlx::postgres::PgRow) -> Result<HoldRecord, AbacusError> {
        let context_json: Option<String> = record.try_get("receipt_context").map_err(storage)?;
        Ok(HoldRecord {
            hold_id: record.try_get("hold_id").map_err(storage)?,
            idem_key: record.try_get("idem_key").map_err(storage)?,
            reserved_micros: record.try_get("reserved_micros").map_err(storage)?,
            created_at_ms: record.try_get("created_at_ms").map_err(storage)?,
            deadline_ms: record.try_get("deadline_ms").map_err(storage)?,
            receipt_context: context_json
                .map(|json| serde_json::from_str(&json))
                .transpose()
                .map_err(|error| {
                    AbacusError::Storage(format!("invalid recovery receipt context: {error}"))
                })?,
        })
    }
}

fn storage(error: sqlx::Error) -> AbacusError {
    AbacusError::Storage(error.to_string())
}

#[async_trait]
impl HoldStore for PostgresHoldStore {
    async fn create(&self, record: HoldRecord) -> Result<(), AbacusError> {
        sqlx::query(
            "INSERT INTO abacus_open_holds \
             (hold_id, idem_key, reserved_micros, created_at_ms, deadline_ms, receipt_context) \
             VALUES ($1, $2, $3, $4, $5, $6::jsonb) ON CONFLICT (hold_id) DO NOTHING",
        )
        .bind(&record.hold_id)
        .bind(&record.idem_key)
        .bind(record.reserved_micros)
        .bind(record.created_at_ms)
        .bind(record.deadline_ms)
        .bind(
            record
                .receipt_context
                .as_ref()
                .map(serde_json::to_string)
                .transpose()
                .map_err(|error| AbacusError::Storage(error.to_string()))?,
        )
        .execute(&self.pool)
        .await
        .map_err(storage)?;

        match self.get(&record.hold_id).await? {
            Some(existing) if existing == record => Ok(()),
            Some(_) => Err(AbacusError::IdempotencyConflict),
            None => Err(AbacusError::Storage(
                "open hold disappeared while registering".to_string(),
            )),
        }
    }

    async fn get(&self, hold_id: &str) -> Result<Option<HoldRecord>, AbacusError> {
        sqlx::query(
            "SELECT hold_id, idem_key, reserved_micros, created_at_ms, deadline_ms, \
             receipt_context::text AS receipt_context \
             FROM abacus_open_holds WHERE hold_id = $1",
        )
        .bind(hold_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        .map(Self::row)
        .transpose()
    }

    async fn list(&self) -> Result<Vec<HoldRecord>, AbacusError> {
        sqlx::query(
            "SELECT hold_id, idem_key, reserved_micros, created_at_ms, deadline_ms, \
             receipt_context::text AS receipt_context \
             FROM abacus_open_holds ORDER BY created_at_ms, hold_id",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?
        .into_iter()
        .map(Self::row)
        .collect()
    }

    async fn claim_expired(
        &self,
        worker_id: &str,
        now_ms: i64,
        ttl_ms: i64,
        lease_until_ms: i64,
        limit: usize,
    ) -> Result<Vec<HoldRecord>, AbacusError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        sqlx::query(
            "WITH candidates AS (SELECT hold_id FROM abacus_open_holds \
             WHERE COALESCE(deadline_ms, created_at_ms + $1) <= $2 \
             AND (lease_until_ms IS NULL OR lease_until_ms <= $2) \
             ORDER BY created_at_ms, hold_id FOR UPDATE SKIP LOCKED LIMIT $3) \
             UPDATE abacus_open_holds h SET lease_owner = $4, lease_until_ms = $5 \
             FROM candidates c WHERE h.hold_id = c.hold_id \
             RETURNING h.hold_id, h.idem_key, h.reserved_micros, \
             h.created_at_ms, h.deadline_ms, h.receipt_context::text AS receipt_context",
        )
        .bind(ttl_ms)
        .bind(now_ms)
        .bind(limit)
        .bind(worker_id)
        .bind(lease_until_ms)
        .fetch_all(&self.pool)
        .await
        .map_err(storage)?
        .into_iter()
        .map(Self::row)
        .collect()
    }

    async fn complete_claim(
        &self,
        hold_id: &str,
        worker_id: &str,
    ) -> Result<Option<HoldRecord>, AbacusError> {
        sqlx::query(
            "DELETE FROM abacus_open_holds WHERE hold_id = $1 AND lease_owner = $2 \
             RETURNING hold_id, idem_key, reserved_micros, created_at_ms, deadline_ms, \
             receipt_context::text AS receipt_context",
        )
        .bind(hold_id)
        .bind(worker_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        .map(Self::row)
        .transpose()
    }

    async fn complete_claim_with_receipt(
        &self,
        hold_id: &str,
        worker_id: &str,
        receipt: &MintReceipt,
        _receipts: &dyn ReceiptStore,
    ) -> Result<Option<HoldRecord>, AbacusError> {
        let mut tx = self.pool.begin().await.map_err(storage)?;
        let removed = sqlx::query(
            "DELETE FROM abacus_open_holds WHERE hold_id = $1 AND lease_owner = $2 \
             RETURNING hold_id, idem_key, reserved_micros, created_at_ms, deadline_ms, \
             receipt_context::text AS receipt_context",
        )
        .bind(hold_id)
        .bind(worker_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(storage)?;
        let Some(removed) = removed else {
            tx.rollback().await.map_err(storage)?;
            return Ok(None);
        };
        let json = serde_json::to_string(receipt)
            .map_err(|error| AbacusError::Storage(error.to_string()))?;
        let inserted = sqlx::query(
            "INSERT INTO abacus_receipts (hold_id, request_id, receipt_json) \
             VALUES ($1, $2, $3::jsonb) ON CONFLICT (hold_id) DO UPDATE SET \
             request_id = EXCLUDED.request_id, receipt_json = EXCLUDED.receipt_json \
             WHERE abacus_receipts.receipt_json = EXCLUDED.receipt_json",
        )
        .bind(&receipt.hold_id)
        .bind(&receipt.request_id)
        .bind(json)
        .execute(&mut *tx)
        .await
        .map_err(storage)?;
        if inserted.rows_affected() == 0 {
            tx.rollback().await.map_err(storage)?;
            return Err(AbacusError::IdempotencyConflict);
        }
        tx.commit().await.map_err(storage)?;
        Ok(Some(Self::row(removed)?))
    }

    async fn remove(&self, hold_id: &str) -> Result<Option<HoldRecord>, AbacusError> {
        sqlx::query(
            "DELETE FROM abacus_open_holds WHERE hold_id = $1 \
             RETURNING hold_id, idem_key, reserved_micros, created_at_ms, deadline_ms, \
             receipt_context::text AS receipt_context",
        )
        .bind(hold_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage)?
        .map(Self::row)
        .transpose()
    }
}
