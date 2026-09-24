//! Postgres implementation of the runtime-editable [`ToolStore`].
//!
//! Runtime query API only (no compile-time `query!` macros). Every write runs in
//! one transaction that row-locks the tool, applies the versioned change and
//! appends the audit entry, so the audit trail can never disagree with the
//! stored state. JSON is bound as TEXT and cast to JSONB in SQL, keeping sqlx's
//! `json` feature out of the build.

use async_trait::async_trait;
use serde_json::Value;
use sqlx::postgres::PgPool;
use sqlx::PgConnection;

use crate::tool_store::{StoredTool, ToolAction, ToolAuditEntry, ToolStore, ToolStoreError};

const COLUMNS: &str = "name, spec_json::text AS spec_json, enabled, deleted, version, \
     (EXTRACT(EPOCH FROM created_at) * 1000)::BIGINT AS created_at_ms, \
     (EXTRACT(EPOCH FROM updated_at) * 1000)::BIGINT AS updated_at_ms";

#[derive(sqlx::FromRow)]
struct Row {
    name: String,
    spec_json: String,
    enabled: bool,
    deleted: bool,
    version: i64,
    created_at_ms: i64,
    updated_at_ms: i64,
}

impl TryFrom<Row> for StoredTool {
    type Error = ToolStoreError;

    fn try_from(row: Row) -> Result<Self, Self::Error> {
        Ok(Self {
            spec: decode(&row.spec_json)?,
            name: row.name,
            enabled: row.enabled,
            deleted: row.deleted,
            version: row.version,
            created_at_ms: row.created_at_ms,
            updated_at_ms: row.updated_at_ms,
        })
    }
}

#[derive(sqlx::FromRow)]
struct AuditRow {
    id: i64,
    tool_name: String,
    action: String,
    actor: String,
    old_spec_json: Option<String>,
    new_spec_json: Option<String>,
    old_enabled: Option<bool>,
    new_enabled: Option<bool>,
    at_ms: i64,
}

fn db(error: sqlx::Error) -> ToolStoreError {
    ToolStoreError::Storage(error.to_string())
}

fn decode(raw: &str) -> Result<Value, ToolStoreError> {
    serde_json::from_str(raw)
        .map_err(|e| ToolStoreError::Storage(format!("invalid stored tool spec: {e}")))
}

fn encode(value: &Value) -> Result<String, ToolStoreError> {
    serde_json::to_string(value).map_err(|e| ToolStoreError::Storage(e.to_string()))
}

async fn locked(conn: &mut PgConnection, name: &str) -> Result<Option<StoredTool>, ToolStoreError> {
    let query = format!("SELECT {COLUMNS} FROM turnstile_paid_tools WHERE name = $1 FOR UPDATE");
    sqlx::query_as::<_, Row>(&query)
        .bind(name)
        .fetch_optional(conn)
        .await
        .map_err(db)?
        .map(TryInto::try_into)
        .transpose()
}

async fn record(
    conn: &mut PgConnection,
    action: ToolAction,
    actor: &str,
    old: Option<&StoredTool>,
    new: &StoredTool,
) -> Result<(), ToolStoreError> {
    let old_spec = old.map(|t| encode(&t.spec)).transpose()?;
    sqlx::query(
        "INSERT INTO turnstile_tool_audit \
         (tool_name, action, actor, old_spec_json, new_spec_json, old_enabled, new_enabled) \
         VALUES ($1, $2, $3, $4::jsonb, $5::jsonb, $6, $7)",
    )
    .bind(&new.name)
    .bind(action.as_str())
    .bind(actor)
    .bind(old_spec)
    .bind(encode(&new.spec)?)
    .bind(old.map(|t| t.enabled))
    .bind(new.enabled)
    .execute(conn)
    .await
    .map_err(db)?;
    Ok(())
}

/// Shared, audited paid-tool store. Cheap to clone.
#[derive(Clone)]
pub struct PostgresToolStore {
    pool: PgPool,
}

impl PostgresToolStore {
    /// Wrap the pool the nonce store already migrated (migration 0004).
    #[must_use]
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Lock a LIVE tool, apply `change`, persist it and append the audit entry.
    async fn mutate(
        &self,
        name: &str,
        action: ToolAction,
        actor: &str,
        expected_version: Option<i64>,
        set_sql: &str,
        spec: Option<&Value>,
    ) -> Result<StoredTool, ToolStoreError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let old = match locked(&mut tx, name).await? {
            Some(tool) if !tool.deleted => tool,
            _ => return Err(ToolStoreError::NotFound(name.to_string())),
        };
        if expected_version.is_some_and(|expected| expected != old.version) {
            return Err(ToolStoreError::VersionConflict {
                name: name.to_string(),
                current: old.version,
            });
        }
        let query = format!(
            "UPDATE turnstile_paid_tools SET {set_sql}, version = version + 1, updated_at = now() \
             WHERE name = $1 RETURNING {COLUMNS}"
        );
        let mut update = sqlx::query_as::<_, Row>(&query).bind(name);
        if let Some(spec) = spec {
            update = update.bind(encode(spec)?);
        }
        let new: StoredTool = update.fetch_one(&mut *tx).await.map_err(db)?.try_into()?;
        record(&mut tx, action, actor, Some(&old), &new).await?;
        tx.commit().await.map_err(db)?;
        Ok(new)
    }
}

#[async_trait]
impl ToolStore for PostgresToolStore {
    async fn list(&self) -> Result<Vec<StoredTool>, ToolStoreError> {
        let query = format!("SELECT {COLUMNS} FROM turnstile_paid_tools ORDER BY name");
        sqlx::query_as::<_, Row>(&query)
            .fetch_all(&self.pool)
            .await
            .map_err(db)?
            .into_iter()
            .map(TryInto::try_into)
            .collect()
    }

    async fn get(&self, name: &str) -> Result<Option<StoredTool>, ToolStoreError> {
        let query = format!("SELECT {COLUMNS} FROM turnstile_paid_tools WHERE name = $1");
        sqlx::query_as::<_, Row>(&query)
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(db)?
            .map(TryInto::try_into)
            .transpose()
    }

    async fn revision(&self) -> Result<i64, ToolStoreError> {
        let (revision,): (i64,) =
            sqlx::query_as("SELECT COALESCE(SUM(version), 0)::BIGINT FROM turnstile_paid_tools")
                .fetch_one(&self.pool)
                .await
                .map_err(db)?;
        Ok(revision)
    }

    async fn create(
        &self,
        name: &str,
        spec: &Value,
        actor: &str,
    ) -> Result<StoredTool, ToolStoreError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        let old = locked(&mut tx, name).await?;
        let row = match &old {
            Some(existing) if !existing.deleted => {
                return Err(ToolStoreError::Conflict(name.to_string()))
            }
            // Revive a soft-deleted name: keep created_at, bump the version.
            Some(_) => {
                let query = format!(
                    "UPDATE turnstile_paid_tools SET spec_json = $2::jsonb, enabled = TRUE, \
                     deleted = FALSE, version = version + 1, updated_at = now() \
                     WHERE name = $1 RETURNING {COLUMNS}"
                );
                sqlx::query_as::<_, Row>(&query)
                    .bind(name)
                    .bind(encode(spec)?)
                    .fetch_optional(&mut *tx)
                    .await
            }
            None => {
                let query = format!(
                    "INSERT INTO turnstile_paid_tools (name, spec_json) VALUES ($1, $2::jsonb) \
                     ON CONFLICT (name) DO NOTHING RETURNING {COLUMNS}"
                );
                sqlx::query_as::<_, Row>(&query)
                    .bind(name)
                    .bind(encode(spec)?)
                    .fetch_optional(&mut *tx)
                    .await
            }
        }
        .map_err(db)?;
        // A concurrent insert of the same name won the race.
        let new: StoredTool = row
            .ok_or_else(|| ToolStoreError::Conflict(name.to_string()))?
            .try_into()?;
        record(&mut tx, ToolAction::Create, actor, old.as_ref(), &new).await?;
        tx.commit().await.map_err(db)?;
        Ok(new)
    }

    async fn update(
        &self,
        name: &str,
        spec: &Value,
        expected_version: Option<i64>,
        actor: &str,
    ) -> Result<StoredTool, ToolStoreError> {
        self.mutate(
            name,
            ToolAction::Update,
            actor,
            expected_version,
            "spec_json = $2::jsonb",
            Some(spec),
        )
        .await
    }

    async fn set_enabled(
        &self,
        name: &str,
        enabled: bool,
        actor: &str,
    ) -> Result<StoredTool, ToolStoreError> {
        let (action, set_sql) = if enabled {
            (ToolAction::Enable, "enabled = TRUE")
        } else {
            (ToolAction::Disable, "enabled = FALSE")
        };
        self.mutate(name, action, actor, None, set_sql, None).await
    }

    async fn delete(&self, name: &str, actor: &str) -> Result<StoredTool, ToolStoreError> {
        self.mutate(
            name,
            ToolAction::Delete,
            actor,
            None,
            "enabled = FALSE, deleted = TRUE",
            None,
        )
        .await
    }

    async fn seed_if_empty(
        &self,
        tools: &[(String, Value)],
        actor: &str,
    ) -> Result<usize, ToolStoreError> {
        let mut tx = self.pool.begin().await.map_err(db)?;
        // Serialize concurrent replica boots so exactly one seeds.
        sqlx::query("LOCK TABLE turnstile_paid_tools IN SHARE ROW EXCLUSIVE MODE")
            .execute(&mut *tx)
            .await
            .map_err(db)?;
        let (existing,): (i64,) = sqlx::query_as("SELECT COUNT(*)::BIGINT FROM turnstile_paid_tools")
            .fetch_one(&mut *tx)
            .await
            .map_err(db)?;
        if existing > 0 {
            return Ok(0);
        }
        let query = format!(
            "INSERT INTO turnstile_paid_tools (name, spec_json) VALUES ($1, $2::jsonb) \
             RETURNING {COLUMNS}"
        );
        for (name, spec) in tools {
            let new: StoredTool = sqlx::query_as::<_, Row>(&query)
                .bind(name)
                .bind(encode(spec)?)
                .fetch_one(&mut *tx)
                .await
                .map_err(db)?
                .try_into()?;
            record(&mut tx, ToolAction::Seed, actor, None, &new).await?;
        }
        tx.commit().await.map_err(db)?;
        Ok(tools.len())
    }

    async fn audit(
        &self,
        tool: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ToolAuditEntry>, ToolStoreError> {
        let rows = sqlx::query_as::<_, AuditRow>(
            "SELECT id, tool_name, action, actor, old_spec_json::text AS old_spec_json, \
             new_spec_json::text AS new_spec_json, old_enabled, new_enabled, \
             (EXTRACT(EPOCH FROM at) * 1000)::BIGINT AS at_ms \
             FROM turnstile_tool_audit WHERE ($1::text IS NULL OR tool_name = $1) \
             ORDER BY id DESC LIMIT $2",
        )
        .bind(tool)
        .bind(i64::try_from(limit).unwrap_or(i64::MAX))
        .fetch_all(&self.pool)
        .await
        .map_err(db)?;
        rows.into_iter()
            .map(|row| {
                Ok(ToolAuditEntry {
                    id: row.id,
                    action: ToolAction::parse(&row.action).ok_or_else(|| {
                        ToolStoreError::Storage(format!("invalid audit action {:?}", row.action))
                    })?,
                    tool: row.tool_name,
                    actor: row.actor,
                    old_spec: row.old_spec_json.as_deref().map(decode).transpose()?,
                    new_spec: row.new_spec_json.as_deref().map(decode).transpose()?,
                    old_enabled: row.old_enabled,
                    new_enabled: row.new_enabled,
                    at_ms: row.at_ms,
                })
            })
            .collect()
    }
}
