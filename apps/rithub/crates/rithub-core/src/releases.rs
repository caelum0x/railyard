//! Releases: a named tag over a commit, with optional notes. The source archive
//! is produced on demand from the git tree (in `rithub-git`), so this module owns
//! only the release metadata.

use crate::error::{CoreError, Result};
use crate::store::PgStore;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// A published release.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Release {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub tag: String,
    pub name: String,
    pub body: Option<String>,
    pub target_sha: String,
    pub author_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Input to create a release.
#[derive(Clone, Debug, Deserialize)]
pub struct NewRelease {
    pub tag: String,
    pub name: String,
    #[serde(default)]
    pub body: Option<String>,
    /// The revision (branch/tag/oid) the release points at.
    pub target: String,
}

fn release_from_row(row: &sqlx::postgres::PgRow) -> Release {
    use sqlx::Row;
    Release {
        id: row.get("id"),
        repo_id: row.get("repo_id"),
        tag: row.get("tag"),
        name: row.get("name"),
        body: row.get("body"),
        target_sha: row.get("target_sha"),
        author_id: row.get("author_id"),
        created_at: row.get("created_at"),
    }
}

/// Release persistence.
#[allow(async_fn_in_trait)]
pub trait ReleaseStore {
    async fn create_release(
        &self,
        repo_id: Uuid,
        tag: &str,
        name: &str,
        body: Option<&str>,
        target_sha: &str,
        author_id: Option<Uuid>,
    ) -> Result<Release>;
    async fn list_releases(&self, repo_id: Uuid) -> Result<Vec<Release>>;
    async fn get_release(&self, repo_id: Uuid, tag: &str) -> Result<Release>;
}

impl ReleaseStore for PgStore {
    async fn create_release(
        &self,
        repo_id: Uuid,
        tag: &str,
        name: &str,
        body: Option<&str>,
        target_sha: &str,
        author_id: Option<Uuid>,
    ) -> Result<Release> {
        let row = sqlx::query(
            "INSERT INTO releases (repo_id, tag, name, body, target_sha, author_id) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             RETURNING id, repo_id, tag, name, body, target_sha, author_id, created_at",
        )
        .bind(repo_id)
        .bind(tag)
        .bind(name)
        .bind(body)
        .bind(target_sha)
        .bind(author_id)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("release", tag, e))?;
        Ok(release_from_row(&row))
    }

    async fn list_releases(&self, repo_id: Uuid) -> Result<Vec<Release>> {
        let rows = sqlx::query(
            "SELECT id, repo_id, tag, name, body, target_sha, author_id, created_at \
             FROM releases WHERE repo_id = $1 ORDER BY created_at DESC",
        )
        .bind(repo_id)
        .fetch_all(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("release", "*", e))?;
        Ok(rows.iter().map(release_from_row).collect())
    }

    async fn get_release(&self, repo_id: Uuid, tag: &str) -> Result<Release> {
        let row = sqlx::query(
            "SELECT id, repo_id, tag, name, body, target_sha, author_id, created_at \
             FROM releases WHERE repo_id = $1 AND tag = $2",
        )
        .bind(repo_id)
        .bind(tag)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("release", tag, e))?;
        Ok(release_from_row(&row))
    }
}
