//! Deploy key persistence + lookup.
//!
//! A deploy key couples an SSH public key (for the SSH transport) with a
//! hashed bearer token (for HTTP Basic auth on the git HTTP transport) and is
//! scoped to exactly one repository. `read_only` gates push access.

use crate::domain::DeployKey;
use crate::error::{CoreError, Result};
use crate::store::PgStore;
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

/// Deploy-key persistence operations.
#[allow(async_fn_in_trait)]
pub trait DeployKeyStore {
    /// Register a new deploy key for a repository. Returns the created row
    /// (without the token_hash — it is stored only in the database).
    async fn create_deploy_key(
        &self,
        repo_id: Uuid,
        title: &str,
        key_blob: &str,
        token_hash: &str,
        read_only: bool,
        created_by: Option<Uuid>,
    ) -> Result<DeployKey>;

    /// List all deploy keys for a repository, oldest first.
    async fn list_deploy_keys(&self, repo_id: Uuid) -> Result<Vec<DeployKey>>;

    /// Delete a deploy key by `(repo_id, key_id)`. Returns `NotFound` if the
    /// key does not exist or does not belong to the given repository.
    async fn delete_deploy_key(&self, repo_id: Uuid, key_id: Uuid) -> Result<()>;

    /// Resolve an SSH public-key blob to its deploy-key record.
    /// Used in the SSH auth path when a connecting client presents a key that
    /// is not in `ssh_keys` (user keys).
    async fn find_deploy_key_by_blob(&self, key_blob: &str) -> Result<Option<DeployKey>>;

    /// Resolve a bearer-token hash to its deploy-key record.
    /// Used in the HTTP Basic auth path.
    async fn find_deploy_key_by_token(&self, token_hash: &str) -> Result<Option<DeployKey>>;
}

// --- row mapping ------------------------------------------------------------

fn deploy_key_from_row(row: &sqlx::postgres::PgRow) -> DeployKey {
    DeployKey {
        id: row.get("id"),
        repo_id: row.get("repo_id"),
        title: row.get("title"),
        key_blob: row.get("key_blob"),
        read_only: row.get("read_only"),
        created_by: row.get("created_by"),
        created_at: row.get::<OffsetDateTime, _>("created_at"),
    }
}

const DK_COLUMNS: &str = "id, repo_id, title, key_blob, read_only, created_by, created_at";

// --- PgStore impl -----------------------------------------------------------

impl DeployKeyStore for PgStore {
    async fn create_deploy_key(
        &self,
        repo_id: Uuid,
        title: &str,
        key_blob: &str,
        token_hash: &str,
        read_only: bool,
        created_by: Option<Uuid>,
    ) -> Result<DeployKey> {
        let row = sqlx::query(&format!(
            "INSERT INTO deploy_keys \
               (repo_id, title, key_blob, token_hash, read_only, created_by) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             RETURNING {DK_COLUMNS}",
        ))
        .bind(repo_id)
        .bind(title)
        .bind(key_blob)
        .bind(token_hash)
        .bind(read_only)
        .bind(created_by)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("deploy_key", title, e))?;
        Ok(deploy_key_from_row(&row))
    }

    async fn list_deploy_keys(&self, repo_id: Uuid) -> Result<Vec<DeployKey>> {
        let rows = sqlx::query(&format!(
            "SELECT {DK_COLUMNS} FROM deploy_keys WHERE repo_id = $1 ORDER BY created_at",
        ))
        .bind(repo_id)
        .fetch_all(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("deploy_key", "*", e))?;
        Ok(rows.iter().map(deploy_key_from_row).collect())
    }

    async fn delete_deploy_key(&self, repo_id: Uuid, key_id: Uuid) -> Result<()> {
        let result = sqlx::query(
            "DELETE FROM deploy_keys WHERE id = $1 AND repo_id = $2",
        )
        .bind(key_id)
        .bind(repo_id)
        .execute(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("deploy_key", &key_id.to_string(), e))?;
        if result.rows_affected() == 0 {
            return Err(CoreError::NotFound {
                kind: "deploy_key",
                key: key_id.to_string(),
            });
        }
        Ok(())
    }

    async fn find_deploy_key_by_blob(&self, key_blob: &str) -> Result<Option<DeployKey>> {
        let row = sqlx::query(&format!(
            "SELECT {DK_COLUMNS} FROM deploy_keys WHERE key_blob = $1",
        ))
        .bind(key_blob)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("deploy_key", "*", e))?;
        Ok(row.as_ref().map(deploy_key_from_row))
    }

    async fn find_deploy_key_by_token(&self, token_hash: &str) -> Result<Option<DeployKey>> {
        let row = sqlx::query(&format!(
            "SELECT {DK_COLUMNS} FROM deploy_keys WHERE token_hash = $1",
        ))
        .bind(token_hash)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("deploy_key", "*", e))?;
        Ok(row.as_ref().map(deploy_key_from_row))
    }
}
