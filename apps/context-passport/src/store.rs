//! Persistence ports plus the production Postgres implementation.
//!
//! Two stores implement one `PassportStore` trait: an in-memory store for tests
//! and embedded demos, and a Postgres store that is the production source of
//! truth. Passport heads, immutable versions, and generation provenance are the
//! three persisted record kinds.

use std::collections::HashMap;
use std::sync::RwLock;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::Row;

use crate::domain::{
    GenerationRecord, Passport, PassportAssets, PassportFilter, PassportKind, PassportVersion,
    VersionStatus,
};
use crate::error::PassportError;

/// Outcome of a create: whether this call actually inserted the passport, plus
/// the head version it created (or found on idempotent replay).
pub struct CreateOutcome {
    pub passport: Passport,
    pub head: PassportVersion,
    pub created: bool,
}

#[async_trait]
pub trait PassportStore: Send + Sync {
    /// Idempotently create a passport and its version 1. Reusing `idempotency_key`
    /// returns the original; the caller must treat `created == false` as a replay.
    async fn create(
        &self,
        idempotency_key: &str,
        passport: &Passport,
        head: &PassportVersion,
    ) -> Result<CreateOutcome, PassportError>;

    async fn get_passport(&self, id: &str) -> Result<Option<Passport>, PassportError>;

    async fn get_version(
        &self,
        id: &str,
        version: i64,
    ) -> Result<Option<PassportVersion>, PassportError>;

    /// The head (highest-numbered) version, draft or locked.
    async fn head_version(&self, id: &str) -> Result<Option<PassportVersion>, PassportError>;

    /// The highest-numbered *locked* version, used by `resolve`.
    async fn latest_locked_version(
        &self,
        id: &str,
    ) -> Result<Option<PassportVersion>, PassportError>;

    /// Persist an updated passport head plus a version snapshot (insert or update).
    /// Used by draft edits, locking, and branching a new version.
    async fn upsert(
        &self,
        passport: &Passport,
        version: &PassportVersion,
    ) -> Result<(), PassportError>;

    async fn list_passports(
        &self,
        filter: &PassportFilter,
    ) -> Result<Vec<Passport>, PassportError>;

    async fn record_generation(&self, record: &GenerationRecord) -> Result<(), PassportError>;

    async fn generations(&self, id: &str) -> Result<Vec<GenerationRecord>, PassportError>;

    async fn ready(&self) -> Result<(), PassportError>;
}

// ---------------------------------------------------------------------------
// In-memory store
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MemoryState {
    passports: HashMap<String, Passport>,
    versions: HashMap<(String, i64), PassportVersion>,
    idempotency: HashMap<String, String>,
    generations: Vec<GenerationRecord>,
}

/// Lightweight store for focused tests and embedded demos.
#[derive(Default)]
pub struct MemoryStore {
    state: RwLock<MemoryState>,
}

impl MemoryStore {
    fn read(&self) -> Result<std::sync::RwLockReadGuard<'_, MemoryState>, PassportError> {
        self.state
            .read()
            .map_err(|_| PassportError::Storage("memory store lock poisoned".into()))
    }

    fn write(&self) -> Result<std::sync::RwLockWriteGuard<'_, MemoryState>, PassportError> {
        self.state
            .write()
            .map_err(|_| PassportError::Storage("memory store lock poisoned".into()))
    }
}

#[async_trait]
impl PassportStore for MemoryStore {
    async fn create(
        &self,
        idempotency_key: &str,
        passport: &Passport,
        head: &PassportVersion,
    ) -> Result<CreateOutcome, PassportError> {
        let mut state = self.write()?;
        if let Some(existing_id) = state.idempotency.get(idempotency_key).cloned() {
            let passport = state
                .passports
                .get(&existing_id)
                .cloned()
                .ok_or_else(|| PassportError::Storage("idempotent passport missing".into()))?;
            let head = state
                .versions
                .get(&(existing_id.clone(), passport.version))
                .cloned()
                .ok_or_else(|| PassportError::Storage("idempotent head missing".into()))?;
            return Ok(CreateOutcome {
                passport,
                head,
                created: false,
            });
        }
        state
            .idempotency
            .insert(idempotency_key.to_string(), passport.id.clone());
        state
            .passports
            .insert(passport.id.clone(), passport.clone());
        state
            .versions
            .insert((passport.id.clone(), head.version), head.clone());
        Ok(CreateOutcome {
            passport: passport.clone(),
            head: head.clone(),
            created: true,
        })
    }

    async fn get_passport(&self, id: &str) -> Result<Option<Passport>, PassportError> {
        Ok(self.read()?.passports.get(id).cloned())
    }

    async fn get_version(
        &self,
        id: &str,
        version: i64,
    ) -> Result<Option<PassportVersion>, PassportError> {
        Ok(self
            .read()?
            .versions
            .get(&(id.to_string(), version))
            .cloned())
    }

    async fn head_version(&self, id: &str) -> Result<Option<PassportVersion>, PassportError> {
        let state = self.read()?;
        Ok(state
            .versions
            .values()
            .filter(|v| v.passport_id == id)
            .max_by_key(|v| v.version)
            .cloned())
    }

    async fn latest_locked_version(
        &self,
        id: &str,
    ) -> Result<Option<PassportVersion>, PassportError> {
        let state = self.read()?;
        Ok(state
            .versions
            .values()
            .filter(|v| v.passport_id == id && v.is_locked())
            .max_by_key(|v| v.version)
            .cloned())
    }

    async fn upsert(
        &self,
        passport: &Passport,
        version: &PassportVersion,
    ) -> Result<(), PassportError> {
        let mut state = self.write()?;
        state
            .passports
            .insert(passport.id.clone(), passport.clone());
        state
            .versions
            .insert((passport.id.clone(), version.version), version.clone());
        Ok(())
    }

    async fn list_passports(
        &self,
        filter: &PassportFilter,
    ) -> Result<Vec<Passport>, PassportError> {
        let limit = filter.limit.unwrap_or(100).clamp(1, 1_000) as usize;
        let mut result: Vec<_> = self
            .read()?
            .passports
            .values()
            .filter(|p| {
                filter
                    .owner
                    .as_ref()
                    .is_none_or(|owner| &p.owner == owner)
                    && filter.kind.as_ref().is_none_or(|kind| &p.kind == kind)
            })
            .cloned()
            .collect();
        result.sort_by_key(|p| std::cmp::Reverse(p.created_at));
        result.truncate(limit);
        Ok(result)
    }

    async fn record_generation(&self, record: &GenerationRecord) -> Result<(), PassportError> {
        self.write()?.generations.push(record.clone());
        Ok(())
    }

    async fn generations(&self, id: &str) -> Result<Vec<GenerationRecord>, PassportError> {
        let mut result: Vec<_> = self
            .read()?
            .generations
            .iter()
            .filter(|g| g.passport_id == id)
            .cloned()
            .collect();
        result.sort_by_key(|g| std::cmp::Reverse(g.created_at));
        Ok(result)
    }

    async fn ready(&self) -> Result<(), PassportError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Postgres store
// ---------------------------------------------------------------------------

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

        // Claim the idempotency key atomically. On conflict, this call is a replay.
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
            let existing_id: String =
                sqlx::query("SELECT passport_id FROM passport_idempotency WHERE idempotency_key = $1")
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
            "SELECT * FROM passport_versions WHERE passport_id = $1 AND version = $2",
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
            "SELECT * FROM passport_versions WHERE passport_id = $1 \
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
            "SELECT * FROM passport_versions WHERE passport_id = $1 AND status = 'locked' \
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

        sqlx::query(
            "INSERT INTO passport_versions \
             (passport_id, version, status, assets, created_at, updated_at, locked_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7) \
             ON CONFLICT (passport_id, version) DO UPDATE SET status = EXCLUDED.status, \
                 assets = EXCLUDED.assets, updated_at = EXCLUDED.updated_at, \
                 locked_at = EXCLUDED.locked_at",
        )
        .bind(&version.passport_id)
        .bind(version.version)
        .bind(version.status.as_str())
        .bind(assets_to_json(&version.assets)?)
        .bind(version.created_at)
        .bind(version.updated_at)
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

    async fn record_generation(&self, record: &GenerationRecord) -> Result<(), PassportError> {
        sqlx::query(
            "INSERT INTO passport_generations \
             (id, passport_id, version, provider, modality, output_ref, created_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7)",
        )
        .bind(&record.id)
        .bind(&record.passport_id)
        .bind(record.version)
        .bind(&record.provider)
        .bind(&record.modality)
        .bind(&record.output_ref)
        .bind(record.created_at)
        .execute(&self.pool)
        .await
        .map_err(storage_error)?;
        Ok(())
    }

    async fn generations(&self, id: &str) -> Result<Vec<GenerationRecord>, PassportError> {
        let rows = sqlx::query(
            "SELECT * FROM passport_generations WHERE passport_id = $1 ORDER BY created_at DESC",
        )
        .bind(id)
        .fetch_all(&self.pool)
        .await
        .map_err(storage_error)?;
        rows.iter().map(row_to_generation).collect()
    }

    async fn ready(&self) -> Result<(), PassportError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(storage_error)?;
        Ok(())
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
    sqlx::query(
        "INSERT INTO passport_versions \
         (passport_id, version, status, assets, created_at, updated_at, locked_at) \
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(&version.passport_id)
    .bind(version.version)
    .bind(version.status.as_str())
    .bind(assets_to_json(&version.assets)?)
    .bind(version.created_at)
    .bind(version.updated_at)
    .bind(version.locked_at)
    .execute(&mut **tx)
    .await
    .map_err(storage_error)?;
    Ok(())
}

fn storage_error(error: sqlx::Error) -> PassportError {
    PassportError::Storage(error.to_string())
}

fn assets_to_json(assets: &PassportAssets) -> Result<serde_json::Value, PassportError> {
    serde_json::to_value(assets)
        .map_err(|error| PassportError::Storage(format!("assets serialize: {error}")))
}

fn assets_from_json(value: serde_json::Value) -> Result<PassportAssets, PassportError> {
    serde_json::from_value(value)
        .map_err(|error| PassportError::Storage(format!("assets deserialize: {error}")))
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
    let status: String = row.try_get("status").map_err(storage_error)?;
    let assets: serde_json::Value = row.try_get("assets").map_err(storage_error)?;
    Ok(PassportVersion {
        passport_id: row.try_get("passport_id").map_err(storage_error)?,
        version: row.try_get("version").map_err(storage_error)?,
        status: VersionStatus::parse(&status)?,
        assets: assets_from_json(assets)?,
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(storage_error)?,
        updated_at: row
            .try_get::<DateTime<Utc>, _>("updated_at")
            .map_err(storage_error)?,
        locked_at: row
            .try_get::<Option<DateTime<Utc>>, _>("locked_at")
            .map_err(storage_error)?,
    })
}

fn row_to_generation(row: &PgRow) -> Result<GenerationRecord, PassportError> {
    Ok(GenerationRecord {
        id: row.try_get("id").map_err(storage_error)?,
        passport_id: row.try_get("passport_id").map_err(storage_error)?,
        version: row.try_get("version").map_err(storage_error)?,
        provider: row.try_get("provider").map_err(storage_error)?,
        modality: row.try_get("modality").map_err(storage_error)?,
        output_ref: row.try_get("output_ref").map_err(storage_error)?,
        created_at: row
            .try_get::<DateTime<Utc>, _>("created_at")
            .map_err(storage_error)?,
    })
}
