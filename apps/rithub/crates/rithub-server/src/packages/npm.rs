//! npm registry: `GET/PUT /api/packages/{org}/{repo}/npm/{name}` (packument,
//! publish) and `GET .../npm/{name}/-/{file}.tgz` (tarball). Scoped names
//! (`@scope/name`, sent as `@scope%2fname`) work. Versions are immutable.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine;
use serde_json::{json, Map, Value};
use sha1::Sha1;
use sha2::{Digest, Sha512};

use super::{access, add_file, base_url, ensure_package, error, files, find_package, get_blob, put_blob, valid_component, Kind};
use crate::state::AppState;

/// The largest tarball accepted (npm's own limit is similar).
const MAX_TARBALL: usize = 256 * 1024 * 1024;

/// A valid npm package name: `name` or `@scope/name`, lowercase.
pub fn valid_name(name: &str) -> bool {
    let ok = |s: &str| !s.is_empty() && !s.starts_with('.') && !s.starts_with('_') && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-._~".contains(&b));
    if name.len() > 214 {
        return false;
    }
    match name.strip_prefix('@') {
        Some(scoped) => matches!(scoped.split_once('/'), Some((scope, pkg)) if ok(scope) && ok(pkg)),
        None => ok(name),
    }
}

enum Target {
    Package(String),
    Tarball(String, String),
}

fn parse(rest: &str) -> Option<Target> {
    let rest = rest.trim_start_matches('/').replace("%2f", "/").replace("%2F", "/");
    match rest.split_once("/-/") {
        Some((name, file)) => (valid_name(name) && valid_component(file)).then(|| Target::Tarball(name.to_string(), file.to_string())),
        None => valid_name(&rest).then_some(Target::Package(rest)),
    }
}

fn json_ok(v: Value) -> Response {
    (StatusCode::OK, Json(v)).into_response()
}

pub async fn get(State(state): State<AppState>, Path((org, repo, rest)): Path<(String, String, String)>, headers: HeaderMap) -> Response {
    let Some(target) = parse(&rest) else {
        return error(StatusCode::NOT_FOUND, "not found");
    };
    let a = match access(&state, &headers, &org, &repo, false).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    match target {
        Target::Package(name) => {
            let Ok(Some((pkg, tags))) = find_package(&state, a.repo.id, Kind::Npm, &name).await else {
                return error(StatusCode::NOT_FOUND, "package not found");
            };
            let rows = files(&state, pkg).await.unwrap_or_default();
            let base = format!("{}/api/packages/{org}/{repo}/npm", base_url(&headers));
            let mut versions = Map::new();
            let mut time = Map::new();
            for f in &rows {
                let mut manifest = f.metadata.clone();
                manifest["dist"]["tarball"] = json!(format!("{base}/{name}/-/{}", f.filename));
                versions.insert(f.version.clone(), manifest);
                time.insert(f.version.clone(), json!(f.created_at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default()));
            }
            let mut dist_tags = tags;
            if dist_tags.get("latest").is_none() {
                if let Some(last) = rows.last() {
                    dist_tags["latest"] = json!(last.version);
                }
            }
            json_ok(json!({ "name": name, "dist-tags": dist_tags, "versions": versions, "time": time }))
        }
        Target::Tarball(name, file) => {
            let Ok(Some((pkg, _))) = find_package(&state, a.repo.id, Kind::Npm, &name).await else {
                return error(StatusCode::NOT_FOUND, "package not found");
            };
            let Some(row) = files(&state, pkg).await.unwrap_or_default().into_iter().find(|f| f.filename == file) else {
                return error(StatusCode::NOT_FOUND, "tarball not found");
            };
            match get_blob(&state, a.repo.id, &row.sha256).await {
                Some(bytes) => {
                    let mut r = (StatusCode::OK, bytes).into_response();
                    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/octet-stream"));
                    r
                }
                None => error(StatusCode::NOT_FOUND, "tarball bytes are missing"),
            }
        }
    }
}

/// `npm publish`: one or more versions with their tarballs as attachments.
pub async fn put(State(state): State<AppState>, Path((org, repo, rest)): Path<(String, String, String)>, headers: HeaderMap, body: Bytes) -> Response {
    let Some(Target::Package(name)) = parse(&rest) else {
        return error(StatusCode::BAD_REQUEST, "invalid package name");
    };
    let a = match access(&state, &headers, &org, &repo, true).await {
        Ok(a) => a,
        Err(r) => return r,
    };
    let Ok(doc) = serde_json::from_slice::<Value>(&body) else {
        return error(StatusCode::BAD_REQUEST, "the publish body is not JSON");
    };
    if doc.get("name").and_then(Value::as_str) != Some(name.as_str()) {
        return error(StatusCode::BAD_REQUEST, "the document name does not match the URL");
    }
    let (Some(versions), Some(attachments)) = (doc.get("versions").and_then(Value::as_object), doc.get("_attachments").and_then(Value::as_object)) else {
        return error(StatusCode::BAD_REQUEST, "versions and _attachments are required");
    };
    if versions.is_empty() {
        return error(StatusCode::BAD_REQUEST, "no version to publish");
    }
    let base_name = name.rsplit('/').next().unwrap_or(&name).to_string();
    let pkg = match ensure_package(&state, a.repo.id, Kind::Npm, &name).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "creating npm package failed");
            return error(StatusCode::INTERNAL_SERVER_ERROR, "publishing failed");
        }
    };
    for (version, manifest) in versions {
        if !valid_component(version) {
            return error(StatusCode::BAD_REQUEST, "invalid version");
        }
        let filename = format!("{base_name}-{version}.tgz");
        let Some(data) = attachments.get(&filename).or_else(|| (attachments.len() == 1).then(|| attachments.values().next()).flatten()).and_then(|v| v.get("data")).and_then(Value::as_str) else {
            return error(StatusCode::BAD_REQUEST, "the tarball attachment is missing");
        };
        let Ok(tarball) = base64::engine::general_purpose::STANDARD.decode(data) else {
            return error(StatusCode::BAD_REQUEST, "the tarball is not base64");
        };
        if tarball.is_empty() || tarball.len() > MAX_TARBALL {
            return error(StatusCode::PAYLOAD_TOO_LARGE, "the tarball is empty or too large");
        }
        let sha = match put_blob(&state, a.repo.id, &tarball).await {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(error = %e, "storing npm tarball failed");
                return error(StatusCode::INTERNAL_SERVER_ERROR, "storage is unavailable");
            }
        };
        let shasum: String = Sha1::digest(&tarball).iter().map(|b| format!("{b:02x}")).collect();
        let integrity = format!("sha512-{}", base64::engine::general_purpose::STANDARD.encode(Sha512::digest(&tarball)));
        let mut stored = manifest.clone();
        stored["name"] = json!(name);
        stored["version"] = json!(version);
        stored["dist"] = json!({ "shasum": shasum, "integrity": integrity });
        match add_file(&state, pkg, version, &filename, &sha, tarball.len() as i64, &stored, a.user_id()).await {
            Ok(true) => {}
            Ok(false) => return error(StatusCode::CONFLICT, "cannot publish over an existing version"),
            Err(e) => {
                tracing::error!(error = %e, "recording npm version failed");
                return error(StatusCode::INTERNAL_SERVER_ERROR, "publishing failed");
            }
        }
        crate::audit::record(&state, Some(a.repo.id), a.user_id(), &a.actor(), "package.published", Some(&format!("npm:{name}@{version}")), None).await;
    }
    if let Some(tags) = doc.get("dist-tags").and_then(Value::as_object) {
        let tags: Map<String, Value> = tags.iter().filter(|(k, v)| valid_component(k) && v.as_str().is_some_and(valid_component)).map(|(k, v)| (k.clone(), v.clone())).collect();
        if let Err(e) = sqlx::query("UPDATE packages SET dist_tags = dist_tags || $2 WHERE id = $1").bind(pkg).bind(Value::Object(tags)).execute(state.store.pool()).await {
            tracing::warn!(error = %e, "updating dist-tags failed");
        }
    }
    (StatusCode::CREATED, Json(json!({ "ok": true, "id": name }))).into_response()
}

#[cfg(test)]
mod tests {
    use super::valid_name;

    #[test]
    fn npm_names() {
        assert!(valid_name("left-pad"));
        assert!(valid_name("@acme/ui"));
        assert!(!valid_name("Left-Pad"));
        assert!(!valid_name("@acme"));
        assert!(!valid_name("../x"));
        assert!(!valid_name("_private"));
    }
}
