//! Package registry: npm, generic files and OCI images, one namespace per
//! repository.
//!
//! | Registry | Base path | Client setup |
//! |---|---|---|
//! | npm | `/api/packages/{org}/{repo}/npm/` | `registry=<base>`, `//host/...:_authToken=<PAT>` |
//! | generic | `/api/packages/{org}/{repo}/generic/{name}/{version}/{file}` | `curl -T file -u user:PAT` |
//! | OCI | `/v2/{org}/{repo}/{image}` | `docker login host -u user -p PAT` |
//!
//! Access follows the repository: anyone who may read it may download, a
//! writer may publish. Personal access tokens (Bearer or Basic) and deploy
//! keys (read-only ones cannot publish) both work. Bytes are stored
//! content-addressed and written atomically after hashing.

pub mod generic;
pub mod npm;
pub mod oci;

use std::path::PathBuf;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use rithub_core::{Repository, Store};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use crate::auth::{authorize_read, authorize_write, Caller};
use crate::state::AppState;

/// The package kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Npm,
    Generic,
    Oci,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Npm => "npm",
            Kind::Generic => "generic",
            Kind::Oci => "oci",
        }
    }
}

/// Who is calling and on which repository, once access is granted.
pub struct Access {
    pub repo: Repository,
    pub caller: Option<Caller>,
}

impl Access {
    pub fn user_id(&self) -> Option<Uuid> {
        self.caller.as_ref().and_then(Caller::user).map(|u| u.id)
    }
    pub fn actor(&self) -> String {
        match &self.caller {
            Some(Caller::User { user, .. }) => user.username.clone(),
            Some(Caller::DeployKey { .. }) => "deploy-key".into(),
            None => "anonymous".into(),
        }
    }
}

/// A 401 that asks for credentials (Basic works for npm, curl and docker).
pub fn challenge() -> Response {
    let mut r = (StatusCode::UNAUTHORIZED, Json(json!({ "error": "authentication required" }))).into_response();
    r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Basic realm=\"rithub\""));
    r
}

pub fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// Resolve the caller (Bearer or Basic) and check read or write access.
pub async fn access(state: &AppState, headers: &HeaderMap, org: &str, repo: &str, write: bool) -> Result<Access, Response> {
    let caller = match crate::authz::api_caller(state, headers).await {
        Ok(c) => c,
        Err(_) => return Err(challenge()),
    };
    let Ok(meta) = state.store.get_repo(org, repo).await else {
        return Err(error(StatusCode::NOT_FOUND, "repository not found"));
    };
    let checked = if write { authorize_write(state, &meta, caller.as_ref()).await } else { authorize_read(state, &meta, caller.as_ref()).await };
    match checked {
        Ok(()) => Ok(Access { repo: meta, caller }),
        // Anonymous callers are asked to sign in rather than told "forbidden".
        Err(_) if caller.is_none() => Err(challenge()),
        Err(r) => Err(r),
    }
}

/// `<name>` path components: letters, digits and `._+-`, not starting with `.`.
pub fn valid_component(s: &str) -> bool {
    !s.is_empty() && s.len() <= 128 && !s.starts_with('.') && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn blob_path(state: &AppState, repo_id: Uuid, hex: &str) -> PathBuf {
    state.data_root.join("packages").join(repo_id.to_string()).join("sha256").join(&hex[..2]).join(hex)
}

/// Store bytes content-addressed; returns the hex sha256.
pub async fn put_blob(state: &AppState, repo_id: Uuid, bytes: &[u8]) -> std::io::Result<String> {
    let hex = sha256_hex(bytes);
    let path = blob_path(state, repo_id, &hex);
    if tokio::fs::metadata(&path).await.is_ok() {
        return Ok(hex);
    }
    let dir = path.parent().ok_or_else(|| std::io::Error::other("bad blob path"))?;
    tokio::fs::create_dir_all(dir).await?;
    let tmp = dir.join(format!(".{hex}.{}.tmp", Uuid::new_v4()));
    tokio::fs::write(&tmp, bytes).await?;
    if let Err(e) = tokio::fs::rename(&tmp, &path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    Ok(hex)
}

pub async fn get_blob(state: &AppState, repo_id: Uuid, hex: &str) -> Option<Vec<u8>> {
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    tokio::fs::read(blob_path(state, repo_id, hex)).await.ok()
}

fn pool(state: &AppState) -> &sqlx::PgPool {
    state.store.pool()
}

/// The package id, created on first publish.
pub async fn ensure_package(state: &AppState, repo_id: Uuid, kind: Kind, name: &str) -> sqlx::Result<Uuid> {
    sqlx::query_scalar(
        "INSERT INTO packages (repo_id, kind, name) VALUES ($1, $2, $3) \
         ON CONFLICT (repo_id, kind, name) DO UPDATE SET name = EXCLUDED.name RETURNING id",
    )
    .bind(repo_id)
    .bind(kind.as_str())
    .bind(name)
    .fetch_one(pool(state))
    .await
}

pub async fn find_package(state: &AppState, repo_id: Uuid, kind: Kind, name: &str) -> sqlx::Result<Option<(Uuid, Value)>> {
    let row = sqlx::query("SELECT id, dist_tags FROM packages WHERE repo_id = $1 AND kind = $2 AND name = $3")
        .bind(repo_id)
        .bind(kind.as_str())
        .bind(name)
        .fetch_optional(pool(state))
        .await?;
    Ok(row.map(|r| (r.get("id"), r.get("dist_tags"))))
}

/// One stored file row.
pub struct FileRow {
    pub version: String,
    pub filename: String,
    pub sha256: String,
    pub size: i64,
    pub metadata: Value,
    pub created_at: time::OffsetDateTime,
}

pub async fn files(state: &AppState, package_id: Uuid) -> sqlx::Result<Vec<FileRow>> {
    let rows = sqlx::query("SELECT version, filename, sha256, size_bytes, metadata, created_at FROM package_files WHERE package_id = $1 ORDER BY created_at, filename")
        .bind(package_id)
        .fetch_all(pool(state))
        .await?;
    Ok(rows
        .iter()
        .map(|r| FileRow {
            version: r.get("version"),
            filename: r.get("filename"),
            sha256: r.get("sha256"),
            size: r.get("size_bytes"),
            metadata: r.get("metadata"),
            created_at: r.get("created_at"),
        })
        .collect())
}

/// Record a file; `false` when that version/filename already exists.
#[allow(clippy::too_many_arguments)]
pub async fn add_file(state: &AppState, package_id: Uuid, version: &str, filename: &str, sha: &str, size: i64, metadata: &Value, user: Option<Uuid>) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "INSERT INTO package_files (package_id, version, filename, sha256, size_bytes, metadata, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (package_id, version, filename) DO NOTHING",
    )
    .bind(package_id)
    .bind(version)
    .bind(filename)
    .bind(sha)
    .bind(size)
    .bind(metadata)
    .bind(user)
    .execute(pool(state))
    .await?;
    Ok(done.rows_affected() == 1)
}

/// `GET /api/packages/{org}/{repo}`: every package with its versions.
pub async fn list_packages(State(state): State<AppState>, Path((org, repo)): Path<(String, String)>, headers: HeaderMap) -> Response {
    let a = match access(&state, &headers, &org, &repo, false).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    let rows = match sqlx::query(
        "SELECT p.kind, p.name, COALESCE(array_agg(DISTINCT f.version) FILTER (WHERE f.version IS NOT NULL), '{}') AS versions, \
                COALESCE(array_agg(DISTINCT m.reference) FILTER (WHERE m.reference IS NOT NULL AND m.reference NOT LIKE 'sha256:%'), '{}') AS tags \
         FROM packages p LEFT JOIN package_files f ON f.package_id = p.id LEFT JOIN oci_manifests m ON m.package_id = p.id \
         WHERE p.repo_id = $1 GROUP BY p.kind, p.name ORDER BY p.kind, p.name",
    )
    .bind(a.repo.id)
    .fetch_all(pool(&state))
    .await
    {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!(error = %e, "listing packages failed");
            return error(StatusCode::INTERNAL_SERVER_ERROR, "listing packages failed");
        }
    };
    let out: Vec<Value> = rows
        .iter()
        .map(|r| {
            let kind: String = r.get("kind");
            let versions: Vec<String> = if kind == "oci" { r.get("tags") } else { r.get("versions") };
            json!({ "kind": kind, "name": r.get::<String, _>("name"), "versions": versions })
        })
        .collect();
    Json(json!({ "packages": out })).into_response()
}

/// The registry base URL for links in responses.
pub fn base_url(headers: &HeaderMap) -> String {
    crate::web::public_base(headers)
}

#[cfg(test)]
mod tests {
    use super::valid_component;

    #[test]
    fn components_are_safe() {
        assert!(valid_component("my-lib_1.2.0+build"));
        assert!(!valid_component("../etc"));
        assert!(!valid_component(".hidden"));
        assert!(!valid_component("a/b"));
        assert!(!valid_component(""));
    }
}
