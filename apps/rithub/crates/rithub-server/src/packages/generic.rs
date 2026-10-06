//! Generic files: `PUT/GET/DELETE /api/packages/{org}/{repo}/generic/{name}/{version}/{file}`.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use super::{access, add_file, ensure_package, error, files, find_package, get_blob, put_blob, valid_component, Kind};
use crate::state::AppState;

type P = Path<(String, String, String, String, String)>;

fn check(name: &str, version: &str, file: &str) -> Option<Response> {
    (!valid_component(name) || !valid_component(version) || !valid_component(file)).then(|| error(StatusCode::BAD_REQUEST, "name, version and file name may use letters, digits and ._+- only"))
}

pub async fn upload(State(state): State<AppState>, Path((org, repo, name, version, file)): P, headers: HeaderMap, body: Bytes) -> Response {
    if let Some(r) = check(&name, &version, &file) {
        return r;
    }
    let a = match access(&state, &headers, &org, &repo, true).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    let sha = match put_blob(&state, a.repo.id, &body).await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "storing package file failed");
            return error(StatusCode::INTERNAL_SERVER_ERROR, "storage is unavailable");
        }
    };
    let added = async {
        let pkg = ensure_package(&state, a.repo.id, Kind::Generic, &name).await?;
        add_file(&state, pkg, &version, &file, &sha, body.len() as i64, &json!({}), a.user_id()).await
    }
    .await;
    match added {
        Ok(true) => {
            crate::audit::record(&state, Some(a.repo.id), a.user_id(), &a.actor(), "package.published", Some(&format!("generic:{name}@{version}/{file}")), None).await;
            (StatusCode::CREATED, Json(json!({ "name": name, "version": version, "file": file, "sha256": sha, "size": body.len() }))).into_response()
        }
        Ok(false) => error(StatusCode::CONFLICT, "this file of this version already exists; delete it first"),
        Err(e) => {
            tracing::error!(error = %e, "recording package file failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "recording the file failed")
        }
    }
}

pub async fn download(State(state): State<AppState>, Path((org, repo, name, version, file)): P, headers: HeaderMap) -> Response {
    if let Some(r) = check(&name, &version, &file) {
        return r;
    }
    let a = match access(&state, &headers, &org, &repo, false).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Ok(Some((pkg, _))) = find_package(&state, a.repo.id, Kind::Generic, &name).await else {
        return error(StatusCode::NOT_FOUND, "package not found");
    };
    let Some(row) = files(&state, pkg).await.unwrap_or_default().into_iter().find(|f| f.version == version && f.filename == file) else {
        return error(StatusCode::NOT_FOUND, "file not found");
    };
    let Some(bytes) = get_blob(&state, a.repo.id, &row.sha256).await else {
        return error(StatusCode::NOT_FOUND, "file bytes are missing");
    };
    let mut r = (StatusCode::OK, bytes).into_response();
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
    if let Ok(v) = HeaderValue::from_str(&format!("sha256={}", row.sha256)) {
        r.headers_mut().insert("digest", v);
    }
    r
}

pub async fn delete(State(state): State<AppState>, Path((org, repo, name, version, file)): P, headers: HeaderMap) -> Response {
    if let Some(r) = check(&name, &version, &file) {
        return r;
    }
    let a = match access(&state, &headers, &org, &repo, true).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Ok(Some((pkg, _))) = find_package(&state, a.repo.id, Kind::Generic, &name).await else {
        return error(StatusCode::NOT_FOUND, "package not found");
    };
    let done = sqlx::query("DELETE FROM package_files WHERE package_id = $1 AND version = $2 AND filename = $3")
        .bind(pkg)
        .bind(&version)
        .bind(&file)
        .execute(state.store.pool())
        .await;
    match done {
        Ok(d) if d.rows_affected() == 1 => {
            crate::audit::record(&state, Some(a.repo.id), a.user_id(), &a.actor(), "package.deleted", Some(&format!("generic:{name}@{version}/{file}")), None).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(_) => error(StatusCode::NOT_FOUND, "file not found"),
        Err(e) => {
            tracing::error!(error = %e, "deleting package file failed");
            error(StatusCode::INTERNAL_SERVER_ERROR, "deleting the file failed")
        }
    }
}
