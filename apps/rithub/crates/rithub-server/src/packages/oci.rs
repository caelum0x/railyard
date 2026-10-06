//! OCI distribution (the subset docker, podman and oras use): `/v2/` ping,
//! blob uploads (monolithic and chunked), blob reads, manifest push/pull by
//! tag or digest, tag listing and deletes. Image names are
//! `{org}/{repo}[/{path}]`; access follows the repository.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

use super::{access, challenge, ensure_package, find_package, get_blob, put_blob, sha256_hex, Access, Kind};
use crate::state::AppState;

const API_VERSION: &str = "registry/2.0";

fn oci_error(status: StatusCode, code: &str, message: &str) -> Response {
    let mut r = (status, Json(json!({ "errors": [{ "code": code, "message": message }] }))).into_response();
    r.headers_mut().insert("docker-distribution-api-version", HeaderValue::from_static(API_VERSION));
    r
}

fn with_headers(mut r: Response, pairs: &[(&'static str, String)]) -> Response {
    r.headers_mut().insert("docker-distribution-api-version", HeaderValue::from_static(API_VERSION));
    for (k, v) in pairs {
        if let Ok(v) = HeaderValue::from_str(v) {
            r.headers_mut().insert(*k, v);
        }
    }
    r
}

/// `GET /v2/`: 200 for a signed-in caller, else a Basic challenge.
pub async fn ping(State(state): State<AppState>, headers: HeaderMap) -> Response {
    match crate::authz::api_caller(&state, &headers).await {
        Ok(Some(_)) => with_headers((StatusCode::OK, Json(json!({}))).into_response(), &[]),
        _ => with_headers(challenge(), &[]),
    }
}

#[derive(Debug, PartialEq)]
enum Route {
    StartUpload,
    Upload(Uuid),
    Blob(String),
    Manifest(String),
    Tags,
}

/// `(org, repo, image, route)` and the full name, from the path after `/v2/`.
fn parse(rest: &str) -> Option<(String, String, String, Route, String)> {
    let segs: Vec<&str> = rest.split('/').collect();
    let n = segs.len();
    let (name_len, route) = if n >= 4 && segs[n - 3] == "blobs" && segs[n - 2] == "uploads" {
        if segs[n - 1].is_empty() { (n - 3, Route::StartUpload) } else { (n - 3, Route::Upload(Uuid::parse_str(segs[n - 1]).ok()?)) }
    } else if n >= 4 && segs[n - 2] == "blobs" && segs[n - 1] == "uploads" {
        (n - 2, Route::StartUpload)
    } else if n >= 4 && segs[n - 2] == "blobs" {
        (n - 2, Route::Blob(segs[n - 1].to_string()))
    } else if n >= 4 && segs[n - 2] == "manifests" {
        (n - 2, Route::Manifest(segs[n - 1].to_string()))
    } else if n >= 4 && segs[n - 2] == "tags" && segs[n - 1] == "list" {
        (n - 2, Route::Tags)
    } else {
        return None;
    };
    if name_len < 2 {
        return None;
    }
    let name = &segs[..name_len];
    let image_ok = name[2..].iter().all(|s| !s.is_empty() && s.len() <= 128 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b)));
    if !image_ok {
        return None;
    }
    let image = if name.len() > 2 { name[2..].join("/") } else { name[1].to_string() };
    Some((name[0].to_string(), name[1].to_string(), image, route, name.join("/")))
}

fn digest_hex(digest: &str) -> Option<&str> {
    digest.strip_prefix("sha256:").filter(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
}

fn upload_path(state: &AppState, repo_id: Uuid, id: Uuid) -> std::path::PathBuf {
    state.data_root.join("packages").join(repo_id.to_string()).join("uploads").join(id.to_string())
}

async fn has_blob(state: &AppState, repo_id: Uuid, digest: &str) -> bool {
    sqlx::query_scalar::<_, i64>("SELECT size_bytes FROM oci_blobs WHERE repo_id = $1 AND digest = $2")
        .bind(repo_id)
        .bind(digest)
        .fetch_optional(state.store.pool())
        .await
        .ok()
        .flatten()
        .is_some()
}

async fn record_blob(state: &AppState, repo_id: Uuid, bytes: &[u8]) -> Result<String, Response> {
    let hex = put_blob(state, repo_id, bytes).await.map_err(|e| {
        tracing::error!(error = %e, "storing OCI blob failed");
        oci_error(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", "storage is unavailable")
    })?;
    let digest = format!("sha256:{hex}");
    sqlx::query("INSERT INTO oci_blobs (repo_id, digest, size_bytes) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
        .bind(repo_id)
        .bind(&digest)
        .bind(bytes.len() as i64)
        .execute(state.store.pool())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "recording OCI blob failed");
            oci_error(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", "recording the blob failed")
        })?;
    Ok(digest)
}

/// Every `/v2/{name}/...` request.
pub async fn dispatch(
    State(state): State<AppState>,
    Path(rest): Path<String>,
    Query(q): Query<std::collections::HashMap<String, String>>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some((org, repo, image, route, name)) = parse(&rest) else {
        return oci_error(StatusCode::NOT_FOUND, "NAME_UNKNOWN", "repository name not known to registry");
    };
    let write = !matches!(method, Method::GET | Method::HEAD);
    let a = match access(&state, &headers, &org, &repo, write).await {
        Ok(a) => a,
        Err(r) => return with_headers(r, &[]),
    };
    match (route, method) {
        (Route::StartUpload, Method::POST) => start_upload(&state, &a, &name, q.get("digest"), &body).await,
        (Route::Upload(id), Method::PATCH) => append(&state, &a, &name, id, &body).await,
        (Route::Upload(id), Method::PUT) => finish(&state, &a, &name, id, q.get("digest"), &body).await,
        (Route::Upload(id), Method::GET) => match tokio::fs::metadata(upload_path(&state, a.repo.id, id)).await {
            Ok(m) => with_headers(StatusCode::NO_CONTENT.into_response(), &[("location", format!("/v2/{name}/blobs/uploads/{id}")), ("range", format!("0-{}", m.len().saturating_sub(1)))]),
            Err(_) => oci_error(StatusCode::NOT_FOUND, "BLOB_UPLOAD_UNKNOWN", "upload not found"),
        },
        (Route::Upload(id), Method::DELETE) => {
            let _ = tokio::fs::remove_file(upload_path(&state, a.repo.id, id)).await;
            with_headers(StatusCode::NO_CONTENT.into_response(), &[])
        }
        (Route::Blob(digest), Method::GET | Method::HEAD) => {
            let Some(hex) = digest_hex(&digest) else {
                return oci_error(StatusCode::BAD_REQUEST, "DIGEST_INVALID", "unsupported digest");
            };
            if !has_blob(&state, a.repo.id, &digest).await {
                return oci_error(StatusCode::NOT_FOUND, "BLOB_UNKNOWN", "blob unknown to registry");
            }
            match get_blob(&state, a.repo.id, hex).await {
                Some(bytes) => {
                    let len = bytes.len();
                    with_headers((StatusCode::OK, bytes).into_response(), &[("docker-content-digest", digest.clone()), ("content-type", "application/octet-stream".into()), ("content-length", len.to_string())])
                }
                None => oci_error(StatusCode::NOT_FOUND, "BLOB_UNKNOWN", "blob bytes are missing"),
            }
        }
        (Route::Blob(digest), Method::DELETE) => {
            let done = sqlx::query("DELETE FROM oci_blobs WHERE repo_id = $1 AND digest = $2").bind(a.repo.id).bind(&digest).execute(state.store.pool()).await;
            match done {
                Ok(d) if d.rows_affected() == 1 => with_headers(StatusCode::ACCEPTED.into_response(), &[]),
                _ => oci_error(StatusCode::NOT_FOUND, "BLOB_UNKNOWN", "blob unknown to registry"),
            }
        }
        (Route::Manifest(reference), Method::PUT) => put_manifest(&state, &a, &name, &image, &reference, &headers, &body).await,
        (Route::Manifest(reference), Method::GET | Method::HEAD) => get_manifest(&state, &a, &image, &reference).await,
        (Route::Manifest(reference), Method::DELETE) => {
            let Ok(Some((pkg, _))) = find_package(&state, a.repo.id, Kind::Oci, &image).await else {
                return oci_error(StatusCode::NOT_FOUND, "NAME_UNKNOWN", "repository name not known to registry");
            };
            // By digest: every tag pointing at it goes; by tag: only the tag.
            let sql = if digest_hex(&reference).is_some() { "DELETE FROM oci_manifests WHERE package_id = $1 AND digest = $2" } else { "DELETE FROM oci_manifests WHERE package_id = $1 AND reference = $2" };
            match sqlx::query(sql).bind(pkg).bind(&reference).execute(state.store.pool()).await {
                Ok(d) if d.rows_affected() > 0 => with_headers(StatusCode::ACCEPTED.into_response(), &[]),
                _ => oci_error(StatusCode::NOT_FOUND, "MANIFEST_UNKNOWN", "manifest unknown"),
            }
        }
        (Route::Tags, Method::GET) => {
            let tags: Vec<String> = match find_package(&state, a.repo.id, Kind::Oci, &image).await {
                Ok(Some((pkg, _))) => sqlx::query_scalar("SELECT reference FROM oci_manifests WHERE package_id = $1 AND reference NOT LIKE 'sha256:%' ORDER BY reference")
                    .bind(pkg)
                    .fetch_all(state.store.pool())
                    .await
                    .unwrap_or_default(),
                _ => return oci_error(StatusCode::NOT_FOUND, "NAME_UNKNOWN", "repository name not known to registry"),
            };
            with_headers(Json(json!({ "name": name, "tags": tags })).into_response(), &[])
        }
        _ => oci_error(StatusCode::METHOD_NOT_ALLOWED, "UNSUPPORTED", "operation not supported"),
    }
}

async fn start_upload(state: &AppState, a: &Access, name: &str, digest: Option<&String>, body: &Bytes) -> Response {
    if let Some(digest) = digest {
        // Monolithic upload in one POST.
        if digest_hex(digest).is_none() || format!("sha256:{}", sha256_hex(body)) != *digest {
            return oci_error(StatusCode::BAD_REQUEST, "DIGEST_INVALID", "provided digest did not match uploaded content");
        }
        return match record_blob(state, a.repo.id, body).await {
            Ok(d) => with_headers(StatusCode::CREATED.into_response(), &[("location", format!("/v2/{name}/blobs/{d}")), ("docker-content-digest", d)]),
            Err(r) => r,
        };
    }
    let id = Uuid::new_v4();
    let path = upload_path(state, a.repo.id, id);
    let created = async {
        tokio::fs::create_dir_all(path.parent().ok_or_else(|| std::io::Error::other("bad path"))?).await?;
        tokio::fs::write(&path, b"").await
    }
    .await;
    if let Err(e) = created {
        tracing::error!(error = %e, "starting OCI upload failed");
        return oci_error(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", "storage is unavailable");
    }
    with_headers(StatusCode::ACCEPTED.into_response(), &[("location", format!("/v2/{name}/blobs/uploads/{id}")), ("range", "0-0".into()), ("docker-upload-uuid", id.to_string())])
}

async fn append(state: &AppState, a: &Access, name: &str, id: Uuid, body: &Bytes) -> Response {
    use tokio::io::AsyncWriteExt;
    let path = upload_path(state, a.repo.id, id);
    let Ok(mut f) = tokio::fs::OpenOptions::new().append(true).open(&path).await else {
        return oci_error(StatusCode::NOT_FOUND, "BLOB_UPLOAD_UNKNOWN", "upload not found");
    };
    if f.write_all(body).await.is_err() || f.flush().await.is_err() {
        return oci_error(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", "storage is unavailable");
    }
    let len = tokio::fs::metadata(&path).await.map(|m| m.len()).unwrap_or(0);
    with_headers(StatusCode::ACCEPTED.into_response(), &[("location", format!("/v2/{name}/blobs/uploads/{id}")), ("range", format!("0-{}", len.saturating_sub(1))), ("docker-upload-uuid", id.to_string())])
}

async fn finish(state: &AppState, a: &Access, name: &str, id: Uuid, digest: Option<&String>, body: &Bytes) -> Response {
    let path = upload_path(state, a.repo.id, id);
    let Ok(mut bytes) = tokio::fs::read(&path).await else {
        return oci_error(StatusCode::NOT_FOUND, "BLOB_UPLOAD_UNKNOWN", "upload not found");
    };
    bytes.extend_from_slice(body);
    let Some(digest) = digest.filter(|d| digest_hex(d).is_some()) else {
        return oci_error(StatusCode::BAD_REQUEST, "DIGEST_INVALID", "a sha256 digest is required");
    };
    if format!("sha256:{}", sha256_hex(&bytes)) != *digest {
        return oci_error(StatusCode::BAD_REQUEST, "DIGEST_INVALID", "provided digest did not match uploaded content");
    }
    let stored = record_blob(state, a.repo.id, &bytes).await;
    let _ = tokio::fs::remove_file(&path).await;
    match stored {
        Ok(d) => with_headers(StatusCode::CREATED.into_response(), &[("location", format!("/v2/{name}/blobs/{d}")), ("docker-content-digest", d)]),
        Err(r) => r,
    }
}

/// Digests a manifest references (config + layers, or an index's manifests).
fn references(m: &Value) -> (Vec<String>, Vec<String>) {
    let digests = |v: Option<&Value>| -> Vec<String> { v.and_then(Value::as_array).map(|a| a.iter().filter_map(|d| d.get("digest").and_then(Value::as_str).map(str::to_string)).collect()).unwrap_or_default() };
    let mut blobs = digests(m.get("layers"));
    if let Some(c) = m.get("config").and_then(|c| c.get("digest")).and_then(Value::as_str) {
        blobs.push(c.to_string());
    }
    (blobs, digests(m.get("manifests")))
}

async fn put_manifest(state: &AppState, a: &Access, name: &str, image: &str, reference: &str, headers: &HeaderMap, body: &Bytes) -> Response {
    let valid_tag = reference.len() <= 128 && !reference.is_empty() && reference.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b)) && !reference.starts_with(['.', '-']);
    if digest_hex(reference).is_none() && !valid_tag {
        return oci_error(StatusCode::BAD_REQUEST, "TAG_INVALID", "invalid tag");
    }
    let Ok(manifest) = serde_json::from_slice::<Value>(body) else {
        return oci_error(StatusCode::BAD_REQUEST, "MANIFEST_INVALID", "manifest is not JSON");
    };
    let media_type = manifest
        .get("mediaType")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).map(str::to_string))
        .unwrap_or_else(|| "application/vnd.oci.image.manifest.v1+json".into());
    let digest = format!("sha256:{}", sha256_hex(body));
    if digest_hex(reference).is_some() && reference != digest {
        return oci_error(StatusCode::BAD_REQUEST, "DIGEST_INVALID", "manifest digest does not match the reference");
    }
    let pkg = match ensure_package(state, a.repo.id, Kind::Oci, image).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "creating OCI package failed");
            return oci_error(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", "recording the manifest failed");
        }
    };
    let (blobs, manifests) = references(&manifest);
    for b in &blobs {
        if !has_blob(state, a.repo.id, b).await {
            return oci_error(StatusCode::BAD_REQUEST, "MANIFEST_BLOB_UNKNOWN", &format!("blob {b} is not in this repository"));
        }
    }
    for m in &manifests {
        let known: Option<String> = sqlx::query_scalar("SELECT digest FROM oci_manifests WHERE package_id = $1 AND digest = $2 LIMIT 1").bind(pkg).bind(m).fetch_optional(state.store.pool()).await.ok().flatten();
        if known.is_none() {
            return oci_error(StatusCode::BAD_REQUEST, "MANIFEST_UNKNOWN", &format!("manifest {m} is not in this repository"));
        }
    }
    if let Err(r) = record_blob(state, a.repo.id, body).await {
        return r;
    }
    let upsert = "INSERT INTO oci_manifests (package_id, reference, digest, media_type) VALUES ($1, $2, $3, $4) \
                  ON CONFLICT (package_id, reference) DO UPDATE SET digest = EXCLUDED.digest, media_type = EXCLUDED.media_type, created_at = now()";
    for r in [reference, digest.as_str()] {
        if let Err(e) = sqlx::query(upsert).bind(pkg).bind(r).bind(&digest).bind(&media_type).execute(state.store.pool()).await {
            tracing::error!(error = %e, "recording OCI manifest failed");
            return oci_error(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", "recording the manifest failed");
        }
    }
    crate::audit::record(state, Some(a.repo.id), a.user_id(), &a.actor(), "package.published", Some(&format!("oci:{image}:{reference}")), None).await;
    with_headers(StatusCode::CREATED.into_response(), &[("location", format!("/v2/{name}/manifests/{digest}")), ("docker-content-digest", digest)])
}

async fn get_manifest(state: &AppState, a: &Access, image: &str, reference: &str) -> Response {
    let Ok(Some((pkg, _))) = find_package(state, a.repo.id, Kind::Oci, image).await else {
        return oci_error(StatusCode::NOT_FOUND, "NAME_UNKNOWN", "repository name not known to registry");
    };
    let row = sqlx::query("SELECT digest, media_type FROM oci_manifests WHERE package_id = $1 AND reference = $2").bind(pkg).bind(reference).fetch_optional(state.store.pool()).await;
    let Ok(Some(row)) = row else {
        return oci_error(StatusCode::NOT_FOUND, "MANIFEST_UNKNOWN", "manifest unknown");
    };
    let digest: String = row.get("digest");
    let media: String = row.get("media_type");
    let Some(bytes) = digest_hex(&digest).map(str::to_string) else {
        return oci_error(StatusCode::NOT_FOUND, "MANIFEST_UNKNOWN", "manifest unknown");
    };
    match get_blob(state, a.repo.id, &bytes).await {
        Some(body) => {
            let len = body.len();
            with_headers((StatusCode::OK, body).into_response(), &[("docker-content-digest", digest), ("content-type", media), ("content-length", len.to_string())])
        }
        None => oci_error(StatusCode::NOT_FOUND, "MANIFEST_UNKNOWN", "manifest bytes are missing"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_parse_from_the_end() {
        let id = Uuid::new_v4();
        assert_eq!(parse("acme/app/web/blobs/uploads/").map(|r| (r.2, r.3)), Some(("web".into(), Route::StartUpload)));
        assert_eq!(parse(&format!("acme/app/a/b/blobs/uploads/{id}")).map(|r| (r.2, r.3)), Some(("a/b".into(), Route::Upload(id))));
        assert_eq!(parse("acme/app/manifests/latest").map(|r| (r.2, r.3)), Some(("app".into(), Route::Manifest("latest".into()))));
        assert_eq!(parse("acme/app/web/tags/list").map(|r| r.3), Some(Route::Tags));
        assert!(parse("acme/blobs/x").is_none());
        assert!(parse("acme/app/Web/manifests/x").is_none());
    }
}
