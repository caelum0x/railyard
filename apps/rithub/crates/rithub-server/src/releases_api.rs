//! JSON API for releases: create, list, and download a source archive. The
//! tarball is generated on demand from the git tree via `rithub-git`.

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue};
use axum::response::Response;
use axum::Json;
use rithub_core::{
    NewRelease, Release, ReleaseAsset, ReleaseAssetStore, ReleaseStore, Store,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;

/// `POST /api/orgs/{org}/repos/{repo}/releases`
pub async fn create_release(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(input): Json<NewRelease>,
) -> Result<Json<Release>, AppError> {
    let meta = state.store.get_repo(&org, &repo).await?;
    let commit = state
        .host
        .resolve_commit(&org, &repo, &input.target)
        .map_err(|_| AppError::bad_request("target revision not found"))?;
    let release = state
        .store
        .create_release(
            meta.id,
            &input.tag,
            &input.name,
            input.body.as_deref(),
            &commit.to_hex(),
            None,
        )
        .await?;
    Ok(Json(release))
}

/// `GET /api/orgs/{org}/repos/{repo}/releases`
pub async fn list_releases(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Result<Json<Vec<Release>>, AppError> {
    let meta = state.store.get_repo(&org, &repo).await?;
    Ok(Json(state.store.list_releases(meta.id).await?))
}

/// `GET /api/orgs/{org}/repos/{repo}/releases/{tag}/archive` — tar.gz download.
pub async fn download_archive(
    State(state): State<AppState>,
    Path((org, repo, tag)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let meta = state.store.get_repo(&org, &repo).await?;
    let release = state.store.get_release(meta.id, &tag).await?;
    let commit = state
        .host
        .resolve_commit(&org, &repo, &release.target_sha)
        .map_err(|_| AppError::not_found("target commit not found"))?;
    let prefix = format!("{repo}-{tag}");
    let bytes = state
        .host
        .archive_targz(&org, &repo, commit, &prefix)
        .map_err(AppError::from)?;

    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/gzip"));
    if let Ok(cd) = HeaderValue::from_str(&format!("attachment; filename=\"{prefix}.tar.gz\"")) {
        response.headers_mut().insert(header::CONTENT_DISPOSITION, cd);
    }
    Ok(response)
}

// ===========================================================================
// Binary release assets (Phase E3)
//
// Upload uses a raw request body plus a `?name=<filename>` query parameter and
// the request's `Content-Type` header — the same shape as GitHub's release
// asset upload API. This is deliberately chosen over `multipart/form-data`: it
// needs no extra dependency (no `multer`), streams a single binary cleanly, and
// matches what release-tooling clients already speak. The bytes are written to
// `<data_root>/release-assets/<release_id>/<asset_id>` and the metadata row
// records the pointer.
// ===========================================================================

/// Query params for an asset upload.
#[derive(Debug, Deserialize)]
pub struct UploadQuery {
    /// The stored filename. Required.
    pub name: String,
}

/// Reject path-traversal / separator characters in an uploaded filename.
fn sanitize_filename(name: &str) -> Result<&str, AppError> {
    let trimmed = name.trim();
    if trimmed.is_empty()
        || trimmed.contains('/')
        || trimmed.contains('\\')
        || trimmed.contains('\0')
        || trimmed == "."
        || trimmed == ".."
    {
        return Err(AppError::bad_request("invalid asset filename"));
    }
    Ok(trimmed)
}

/// `POST /api/orgs/{org}/repos/{repo}/releases/{tag}/assets?name=<filename>`
///
/// The raw request body is the file content; `Content-Type` is preserved.
pub async fn upload_asset(
    State(state): State<AppState>,
    Path((org, repo, tag)): Path<(String, String, String)>,
    Query(query): Query<UploadQuery>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<ReleaseAsset>, AppError> {
    let meta = state.store.get_repo(&org, &repo).await?;
    let release = state.store.get_release(meta.id, &tag).await?;
    let filename = sanitize_filename(&query.name)?;

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .unwrap_or("application/octet-stream")
        .to_string();

    // Write the blob under the per-release directory, keyed by a fresh id so two
    // uploads never collide on disk even before the DB uniqueness check.
    let asset_id = Uuid::new_v4();
    let dir = state.release_asset_dir(release.id);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::internal(format!("creating asset dir: {e}")))?;
    let path = dir.join(asset_id.to_string());
    let size = body.len() as i64;
    tokio::fs::write(&path, &body)
        .await
        .map_err(|e| AppError::internal(format!("writing asset blob: {e}")))?;

    let storage_path = path.to_string_lossy().into_owned();
    // On a metadata conflict (duplicate filename) roll back the orphaned blob.
    let asset = match state
        .store
        .create_asset(release.id, filename, &content_type, size, &storage_path)
        .await
    {
        Ok(a) => a,
        Err(e) => {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(e.into());
        }
    };

    crate::audit::record(
        &state,
        Some(meta.id),
        None,
        "release",
        "asset.uploaded",
        Some(filename),
        Some(&format!("release={tag} bytes={size}")),
    )
    .await;
    Ok(Json(asset))
}

/// `GET /api/orgs/{org}/repos/{repo}/releases/{tag}/assets` — list assets.
pub async fn list_assets(
    State(state): State<AppState>,
    Path((org, repo, tag)): Path<(String, String, String)>,
) -> Result<Json<Vec<ReleaseAsset>>, AppError> {
    let meta = state.store.get_repo(&org, &repo).await?;
    let release = state.store.get_release(meta.id, &tag).await?;
    Ok(Json(state.store.list_assets(release.id).await?))
}

/// `GET /api/orgs/{org}/repos/{repo}/releases/{tag}/assets/{asset_id}` — download.
pub async fn download_asset(
    State(state): State<AppState>,
    Path((org, repo, tag, asset_id)): Path<(String, String, String, Uuid)>,
) -> Result<Response, AppError> {
    let meta = state.store.get_repo(&org, &repo).await?;
    let release = state.store.get_release(meta.id, &tag).await?;
    let asset = state.store.get_asset(release.id, asset_id).await?;

    let bytes = tokio::fs::read(&asset.storage_path)
        .await
        .map_err(|_| AppError::not_found("asset blob missing"))?;

    let mut response = Response::new(Body::from(bytes));
    if let Ok(ct) = HeaderValue::from_str(&asset.content_type) {
        response.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    if let Ok(cd) =
        HeaderValue::from_str(&format!("attachment; filename=\"{}\"", asset.filename))
    {
        response.headers_mut().insert(header::CONTENT_DISPOSITION, cd);
    }
    Ok(response)
}

/// `DELETE /api/orgs/{org}/repos/{repo}/releases/{tag}/assets/{asset_id}`
pub async fn delete_asset(
    State(state): State<AppState>,
    Path((org, repo, tag, asset_id)): Path<(String, String, String, Uuid)>,
) -> Result<axum::http::StatusCode, AppError> {
    let meta = state.store.get_repo(&org, &repo).await?;
    let release = state.store.get_release(meta.id, &tag).await?;
    let asset = state.store.delete_asset(release.id, asset_id).await?;
    // Best-effort blob cleanup — the row is already gone, so a stale file only
    // wastes disk, never leaks access.
    let _ = tokio::fs::remove_file(&asset.storage_path).await;
    crate::audit::record(
        &state,
        Some(meta.id),
        None,
        "release",
        "asset.deleted",
        Some(&asset.filename),
        None,
    )
    .await;
    Ok(axum::http::StatusCode::NO_CONTENT)
}
