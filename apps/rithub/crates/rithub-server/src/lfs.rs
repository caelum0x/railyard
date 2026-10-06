//! Git LFS: the batch API and the `basic` transfer adapter.
//!
//! | Method + path (under `/{owner}/{repo}.git/`) | Purpose | Auth |
//! |---|---|---|
//! | `POST info/lfs/objects/batch` | negotiate uploads / downloads | read (download), write (upload) |
//! | `GET  info/lfs/objects/{oid}` | download an object | read |
//! | `PUT  info/lfs/objects/{oid}` | upload an object (sha256-verified) | write |
//!
//! Objects live at `$RITHUB_DATA_ROOT/lfs/{repo_id}/{oid[0..2]}/{oid[2..4]}/{oid}`
//! and are written atomically (temp file + rename) only after the content
//! hashes to its oid. Batch actions carry the caller's own `Authorization`
//! header, so transfers authenticate exactly like the batch call did.

use std::path::PathBuf;

use axum::body::Bytes;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use rithub_core::Repository as RepoMeta;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::auth::{authorize_read, authorize_write, Caller};
use crate::state::AppState;

/// The LFS media type.
pub const LFS_JSON: &str = "application/vnd.git-lfs+json";
/// The largest object accepted (bounded by the git body limit).
pub const MAX_OBJECT_BYTES: u64 = crate::MAX_GIT_BODY_BYTES as u64;
/// Objects per batch request.
pub const MAX_BATCH_OBJECTS: usize = 1000;
/// How long batch action links are advertised as valid.
const ACTION_TTL_S: u64 = 3600;

#[derive(Debug, Deserialize)]
struct BatchRequest {
    operation: String,
    #[serde(default)]
    transfers: Option<Vec<String>>,
    objects: Vec<BatchObject>,
}

#[derive(Debug, Deserialize)]
struct BatchObject {
    oid: String,
    size: i64,
}

/// A 64-character lowercase hex SHA-256 object id.
pub fn valid_oid(oid: &str) -> bool {
    oid.len() == 64 && oid.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn object_path(state: &AppState, repo: &RepoMeta, oid: &str) -> PathBuf {
    state.data_root.join("lfs").join(repo.id.to_string()).join(&oid[..2]).join(&oid[2..4]).join(oid)
}

fn lfs_json(code: StatusCode, body: Value) -> Response {
    let mut r = (code, body.to_string()).into_response();
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(LFS_JSON));
    r
}

fn lfs_error(code: StatusCode, message: &str) -> Response {
    lfs_json(code, json!({ "message": message, "documentation_url": "https://github.com/git-lfs/git-lfs/blob/main/docs/api/batch.md" }))
}

/// `POST info/lfs/objects/batch`.
pub async fn batch(state: &AppState, owner: &str, repo_name: &str, meta: &RepoMeta, caller: Option<&Caller>, headers: &HeaderMap, body: &Bytes) -> Response {
    let Ok(req) = serde_json::from_slice::<BatchRequest>(body) else {
        return lfs_error(StatusCode::UNPROCESSABLE_ENTITY, "malformed batch request");
    };
    if req.transfers.as_ref().is_some_and(|t| !t.iter().any(|x| x == "basic")) {
        return lfs_error(StatusCode::UNPROCESSABLE_ENTITY, "only the basic transfer adapter is supported");
    }
    if req.objects.len() > MAX_BATCH_OBJECTS {
        return lfs_error(StatusCode::UNPROCESSABLE_ENTITY, "too many objects in one batch");
    }
    let upload = match req.operation.as_str() {
        "upload" => true,
        "download" => false,
        _ => return lfs_error(StatusCode::UNPROCESSABLE_ENTITY, "operation must be upload or download"),
    };
    let authorized = if upload { authorize_write(state, meta, caller).await } else { authorize_read(state, meta, caller).await };
    if let Err(r) = authorized {
        return r;
    }
    let href_base = format!("{}/{owner}/{repo_name}.git/info/lfs/objects", crate::web::public_base(headers));
    let auth_header = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).map(str::to_string);
    let action = |oid: &str| {
        let mut a = json!({ "href": format!("{href_base}/{oid}"), "expires_in": ACTION_TTL_S });
        if let Some(h) = &auth_header {
            a["header"] = json!({ "Authorization": h });
        }
        a
    };
    let objects: Vec<Value> = req
        .objects
        .iter()
        .map(|o| {
            if !valid_oid(&o.oid) || o.size < 0 {
                return json!({ "oid": o.oid, "size": o.size, "error": { "code": 422, "message": "invalid oid or size" } });
            }
            if o.size as u64 > MAX_OBJECT_BYTES {
                return json!({ "oid": o.oid, "size": o.size, "error": { "code": 422, "message": "object is larger than this server accepts" } });
            }
            let present = std::fs::metadata(object_path(state, meta, &o.oid)).map(|m| m.len() == o.size as u64).unwrap_or(false);
            match (upload, present) {
                // Already stored: the client skips the upload.
                (true, true) => json!({ "oid": o.oid, "size": o.size, "authenticated": true }),
                (true, false) => json!({ "oid": o.oid, "size": o.size, "authenticated": true, "actions": { "upload": action(&o.oid) } }),
                (false, true) => json!({ "oid": o.oid, "size": o.size, "authenticated": true, "actions": { "download": action(&o.oid) } }),
                (false, false) => json!({ "oid": o.oid, "size": o.size, "error": { "code": 404, "message": "object does not exist" } }),
            }
        })
        .collect();
    lfs_json(StatusCode::OK, json!({ "transfer": "basic", "objects": objects, "hash_algo": "sha256" }))
}

/// `GET info/lfs/objects/{oid}`.
pub async fn download(state: &AppState, meta: &RepoMeta, caller: Option<&Caller>, oid: &str) -> Response {
    if let Err(r) = authorize_read(state, meta, caller).await {
        return r;
    }
    if !valid_oid(oid) {
        return lfs_error(StatusCode::NOT_FOUND, "object does not exist");
    }
    match tokio::fs::read(object_path(state, meta, oid)).await {
        Ok(bytes) => {
            let mut r = (StatusCode::OK, bytes).into_response();
            r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
            r
        }
        Err(_) => lfs_error(StatusCode::NOT_FOUND, "object does not exist"),
    }
}

/// `PUT info/lfs/objects/{oid}`: stored only when the content hashes to `oid`.
pub async fn upload(state: &AppState, meta: &RepoMeta, caller: Option<&Caller>, oid: &str, body: &Bytes) -> Response {
    if let Err(r) = authorize_write(state, meta, caller).await {
        return r;
    }
    if !valid_oid(oid) {
        return lfs_error(StatusCode::UNPROCESSABLE_ENTITY, "invalid oid");
    }
    let digest = Sha256::digest(body);
    let actual: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    if actual != oid {
        return lfs_error(StatusCode::UNPROCESSABLE_ENTITY, "content does not match its oid");
    }
    let path = object_path(state, meta, oid);
    let Some(dir) = path.parent() else {
        return lfs_error(StatusCode::INTERNAL_SERVER_ERROR, "storage path is invalid");
    };
    if let Err(e) = tokio::fs::create_dir_all(dir).await {
        tracing::error!(error = %e, "lfs: creating object directory failed");
        return lfs_error(StatusCode::INTERNAL_SERVER_ERROR, "storage is unavailable");
    }
    let tmp = dir.join(format!(".{oid}.{}.tmp", uuid::Uuid::new_v4()));
    let written = async {
        tokio::fs::write(&tmp, body).await?;
        tokio::fs::rename(&tmp, &path).await
    }
    .await;
    if let Err(e) = written {
        let _ = tokio::fs::remove_file(&tmp).await;
        tracing::error!(error = %e, "lfs: writing object failed");
        return lfs_error(StatusCode::INTERNAL_SERVER_ERROR, "storage is unavailable");
    }
    (StatusCode::OK, ()).into_response()
}

#[cfg(test)]
mod tests {
    use super::valid_oid;

    #[test]
    fn oids_are_lowercase_sha256_hex() {
        assert!(valid_oid(&"a".repeat(64)));
        assert!(!valid_oid(&"A".repeat(64)));
        assert!(!valid_oid(&"a".repeat(63)));
        assert!(!valid_oid("../../etc/passwd"));
    }
}
