//! JSON API for releases: create, list, and download a source archive. The
//! tarball is generated on demand from the git tree via `rithub-git`.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderValue};
use axum::response::Response;
use axum::Json;
use rithub_core::{NewRelease, Release, ReleaseStore, Store};

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
