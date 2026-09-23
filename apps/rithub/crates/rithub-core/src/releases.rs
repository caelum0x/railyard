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

// ---------------------------------------------------------------------------
// Release assets: uploaded binaries attached to a release. The bytes live on
// the data-root filesystem; this owns only the metadata + storage pointer.
// ---------------------------------------------------------------------------

/// A binary asset attached to a release.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReleaseAsset {
    pub id: Uuid,
    pub release_id: Uuid,
    pub filename: String,
    pub content_type: String,
    pub size_bytes: i64,
    /// Absolute on-disk path. Not serialized: an internal storage detail.
    #[serde(skip_serializing)]
    pub storage_path: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

fn asset_from_row(row: &sqlx::postgres::PgRow) -> ReleaseAsset {
    use sqlx::Row;
    ReleaseAsset {
        id: row.get("id"),
        release_id: row.get("release_id"),
        filename: row.get("filename"),
        content_type: row.get("content_type"),
        size_bytes: row.get("size_bytes"),
        storage_path: row.get("storage_path"),
        created_at: row.get("created_at"),
    }
}

/// Release-asset persistence.
#[allow(async_fn_in_trait)]
pub trait ReleaseAssetStore {
    async fn create_asset(
        &self,
        release_id: Uuid,
        filename: &str,
        content_type: &str,
        size_bytes: i64,
        storage_path: &str,
    ) -> Result<ReleaseAsset>;
    async fn list_assets(&self, release_id: Uuid) -> Result<Vec<ReleaseAsset>>;
    async fn get_asset(&self, release_id: Uuid, asset_id: Uuid) -> Result<ReleaseAsset>;
    /// Delete an asset row, returning it so the caller can unlink the blob.
    async fn delete_asset(&self, release_id: Uuid, asset_id: Uuid) -> Result<ReleaseAsset>;
}

impl ReleaseAssetStore for PgStore {
    async fn create_asset(
        &self,
        release_id: Uuid,
        filename: &str,
        content_type: &str,
        size_bytes: i64,
        storage_path: &str,
    ) -> Result<ReleaseAsset> {
        let row = sqlx::query(
            "INSERT INTO release_assets (release_id, filename, content_type, size_bytes, storage_path) \
             VALUES ($1, $2, $3, $4, $5) \
             RETURNING id, release_id, filename, content_type, size_bytes, storage_path, created_at",
        )
        .bind(release_id)
        .bind(filename)
        .bind(content_type)
        .bind(size_bytes)
        .bind(storage_path)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("release_asset", filename, e))?;
        Ok(asset_from_row(&row))
    }

    async fn list_assets(&self, release_id: Uuid) -> Result<Vec<ReleaseAsset>> {
        let rows = sqlx::query(
            "SELECT id, release_id, filename, content_type, size_bytes, storage_path, created_at \
             FROM release_assets WHERE release_id = $1 ORDER BY created_at",
        )
        .bind(release_id)
        .fetch_all(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("release_asset", "*", e))?;
        Ok(rows.iter().map(asset_from_row).collect())
    }

    async fn get_asset(&self, release_id: Uuid, asset_id: Uuid) -> Result<ReleaseAsset> {
        let row = sqlx::query(
            "SELECT id, release_id, filename, content_type, size_bytes, storage_path, created_at \
             FROM release_assets WHERE release_id = $1 AND id = $2",
        )
        .bind(release_id)
        .bind(asset_id)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("release_asset", &asset_id.to_string(), e))?;
        Ok(asset_from_row(&row))
    }

    async fn delete_asset(&self, release_id: Uuid, asset_id: Uuid) -> Result<ReleaseAsset> {
        let row = sqlx::query(
            "DELETE FROM release_assets WHERE release_id = $1 AND id = $2 \
             RETURNING id, release_id, filename, content_type, size_bytes, storage_path, created_at",
        )
        .bind(release_id)
        .bind(asset_id)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("release_asset", &asset_id.to_string(), e))?;
        Ok(asset_from_row(&row))
    }
}
