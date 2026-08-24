//! Production Postgres store. Implements both the `PassportStore` and
//! `ProvenanceLog` ports using the sqlx RUNTIME query API (no `query!` macros),
//! so this crate compiles with no `DATABASE_URL` and is live-tested at deploy.

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::Row;

use passport_core::{
    CreateOutcome, Passport, PassportError, PassportFilter, PassportKind, PassportStore,
    PassportVersion, VersionStatus,
};
use passport_provenance::{GenerationRecord, ProvenanceLog};

#[derive(Clone)]
pub struct PostgresStore {
    pool: PgPool,
}

impl PostgresStore {
    pub async fn connect(database_url: &str) -> Result<Self, PassportError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await
            .map_err(storage_error)?;
        Ok(Self { pool })
    }

    pub async fn migrate(&self) -> Result<(), PassportError> {
        sqlx::migrate!()
            .run(&self.pool)
            .await
            .map_err(|error| PassportError::Storage(error.to_string()))
    }
}

#[async_trait]
impl PassportStore for PostgresStore {
    async fn create(
        &self,
        idempotency_key: &str,
        passport: &Passport,
        head: &PassportVersion,
    ) -> Result<CreateOutcome, PassportError> {
        let mut tx = self.pool.begin().await.map_err(storage_error)?;

        let claimed = sqlx::query(
            "INSERT INTO passport_idempotency (idempotency_key, passport_id) VALUES ($1, $2) \
             ON CONFLICT (idempotency_key) DO NOTHING",
        )
        .bind(idempotency_key)
        .bind(&passport.id)
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?;

        if claimed.rows_affected() == 0 {
            tx.rollback().await.map_err(storage_error)?;
            let existing_id: String = sqlx::query(
                "SELECT passport_id FROM passport_idempotency WHERE idempotency_key = $1",
            )
            .bind(idempotency_key)
            .fetch_one(&self.pool)
            .await
            .map_err(storage_error)?
            .try_get("passport_id")
            .map_err(storage_error)?;
            let passport = self
                .get_passport(&existing_id)
                .await?
                .ok_or_else(|| PassportError::Storage("idempotent passport missing".into()))?;
            let head = self
                .head_version(&existing_id)
                .await?
                .ok_or_else(|| PassportError::Storage("idempotent head missing".into()))?;
            return Ok(CreateOutcome {
                passport,
                head,
                created: false,
            });
        }

        insert_passport(&mut tx, passport).await?;
        insert_version(&mut tx, head).await?;
        tx.commit().await.map_err(storage_error)?;
        Ok(CreateOutcome {
            passport: passport.clone(),
            head: head.clone(),
            created: true,
        })
    }

    async fn get_passport(&self, id: &str) -> Result<Option<Passport>, PassportError> {
        let row = sqlx::query("SELECT * FROM passports WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage_error)?;
        row.as_ref().map(row_to_passport).transpose()
    }

    async fn get_version(
        &self,
        id: &str,
        version: i64,
    ) -> Result<Option<PassportVersion>, PassportError> {
        let row = sqlx::query(
            "SELECT snapshot FROM passport_versions WHERE passport_id = $1 AND version = $2",
        )
        .bind(id)
        .bind(version)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?;
        row.as_ref().map(row_to_version).transpose()
    }

    async fn head_version(&self, id: &str) -> Result<Option<PassportVersion>, PassportError> {
        let row = sqlx::query(
            "SELECT snapshot FROM passport_versions WHERE passport_id = $1 \
             ORDER BY version DESC LIMIT 1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?;
        row.as_ref().map(row_to_version).transpose()
    }

    async fn latest_locked_version(
        &self,
        id: &str,
    ) -> Result<Option<PassportVersion>, PassportError> {
        let row = sqlx::query(
            "SELECT snapshot FROM passport_versions WHERE passport_id = $1 AND status = 'locked' \
             ORDER BY version DESC LIMIT 1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(storage_error)?;
        row.as_ref().map(row_to_version).transpose()
    }

    async fn upsert(
        &self,
        passport: &Passport,
        version: &PassportVersion,
    ) -> Result<(), PassportError> {
        let mut tx = self.pool.begin().await.map_err(storage_error)?;
        sqlx::query(
            "INSERT INTO passports (id, kind, owner, status, version, created_at, updated_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) \
             ON CONFLICT (id) DO UPDATE SET status = EXCLUDED.status, \
                 version = EXCLUDED.version, updated_at = EXCLUDED.updated_at",
        )
        .bind(&passport.id)
        .bind(passport.kind.as_str())
        .bind(&passport.owner)
        .bind(passport.status.as_str())
        .bind(passport.version)
        .bind(passport.created_at)
        .bind(passport.updated_at)
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?;

        let snapshot = serde_json::to_value(version)
            .map_err(|e| PassportError::Storage(format!("version serialize: {e}")))?;
        sqlx::query(
            "INSERT INTO passport_versions (passport_id, version, status, snapshot, created_at, locked_at) \
             VALUES ($1,$2,$3,$4,$5,$6) \
             ON CONFLICT (passport_id, version) DO UPDATE SET status = EXCLUDED.status, \
                 snapshot = EXCLUDED.snapshot, locked_at = EXCLUDED.locked_at",
        )
        .bind(&version.passport_id)
        .bind(version.version)
        .bind(version.status.as_str())
        .bind(snapshot)
        .bind(version.created_at)
        .bind(version.locked_at)
        .execute(&mut *tx)
        .await
        .map_err(storage_error)?;

        tx.commit().await.map_err(storage_error)?;
        Ok(())
    }

    async fn list_passports(
        &self,
        filter: &PassportFilter,
    ) -> Result<Vec<Passport>, PassportError> {
        let limit = filter.limit.unwrap_or(100).clamp(1, 1_000);
        let kind = filter.kind.map(|k| k.as_str().to_string());
        let rows = sqlx::query(
            "SELECT * FROM passports \
             WHERE ($1::TEXT IS NULL OR owner = $1) \
               AND ($2::TEXT IS NULL OR kind = $2) \
             ORDER BY created_at DESC LIMIT $3",
        )
        .bind(&filter.owner)
        .bind(&kind)
        .bind(limit)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?;
        rows.iter().map(row_to_passport).collect()
    }

    async fn ready(&self) -> Result<(), PassportError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(storage_error)?;
        Ok(())
    }
}

#[async_trait]
impl ProvenanceLog for PostgresStore {
    async fn record(&self, record: &GenerationRecord) -> Result<(), PassportError> {
        let snapshot = serde_json::to_value(record)
            .map_err(|e| PassportError::Storage(format!("generation serialize: {e}")))?;
        sqlx::query(
            "INSERT INTO passport_generations (id, passport_id, snapshot, created_at) \
             VALUES ($1,$2,$3,$4) ON CONFLICT (id) DO NOTHING",
        )
        .bind(&record.id)
        .bind(&record.passport_id)
        .bind(snapshot)
        .bind(record.created_at)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(())
    }

    async fn list(&self, passport_id: &str) -> Result<Vec<GenerationRecord>, PassportError> {
        let rows = sqlx::query(
            "SELECT snapshot FROM passport_generations WHERE passport_id = $1 \
             ORDER BY created_at DESC",
        )
        .bind(passport_id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?;
        rows.iter().map(row_to_generation).collect()
    }
}

async fn insert_passport(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    passport: &Passport,
) -> Result<(), PassportError> {
    sqlx::query(
        "INSERT INTO passports (id, kind, owner, status, version, created_at, updated_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(&passport.id)
    .bind(passport.kind.as_str())
    .bind(&passport.owner)
    .bind(passport.status.as_str())
    .bind(passport.version)
    .bind(passport.created_at)
    .bind(passport.updated_at)
    .execute(&mut **tx)
    .await
    .map_err(storage_error)?;
    Ok(())
}

async fn insert_version(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    version: &PassportVersion,
) -> Result<(), PassportError> {
    let snapshot = serde_json::to_value(version)
        .map_err(|e| PassportError::Storage(format!("version serialize: {e}")))?;
    sqlx::query(
        "INSERT INTO passport_versions (passport_id, version, status, snapshot, created_at, locked_at) \
         VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(&version.passport_id)
    .bind(version.version)
    .bind(version.status.as_str())
    .bind(snapshot)
    .bind(version.created_at)
    .bind(version.locked_at)
    .execute(&mut **tx)
    .await
    .map_err(storage_error)?;
    Ok(())
}

fn storage_error(error: sqlx::Error) -> PassportError {
    PassportError::Storage(error.to_string())
}

fn row_to_passport(row: &PgRow) -> Result<Passport, PassportError> {
    let kind: String = row.try_get("kind").map_err(storage_error)?;
    let status: String = row.try_get("status").map_err(storage_error)?;
    Ok(Passport {
        id: row.try_get("id").map_err(storage_error)?,
        kind: PassportKind::parse(&kind)?,
        owner: row.try_get("owner").map_err(storage_error)?,
        status: VersionStatus::parse(&status)?,
        version: row.try_get("version").map_err(storage_error)?,
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(storage_error)?,
        updated_at: row
            .try_get::<DateTime<Utc>, _>("updated_at")
            .map_err(storage_error)?,
    })
}

fn row_to_version(row: &PgRow) -> Result<PassportVersion, PassportError> {
    let snapshot: serde_json::Value = row.try_get("snapshot").map_err(storage_error)?;
    serde_json::from_value(snapshot)
        .map_err(|e| PassportError::Storage(format!("version deserialize: {e}")))
}

fn row_to_generation(row: &PgRow) -> Result<GenerationRecord, PassportError> {
    let snapshot: serde_json::Value = row.try_get("snapshot").map_err(storage_error)?;
    serde_json::from_value(snapshot)
        .map_err(|e| PassportError::Storage(format!("generation deserialize: {e}")))
}
