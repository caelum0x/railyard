//! The git HTTP transport: **smart** protocol (clone/fetch via `git-upload-pack`
//! and push via `git-receive-pack`) with the **dumb** protocol retained as a
//! fallback. Every request is authorized before any object or ref is touched.
//!
//! Routing (both methods share the `/{owner}/{repo}/{*path}` catch-all):
//!
//! | Method + path | Handler | Auth |
//! |---------------|---------|------|
//! | `GET  info/refs?service=git-upload-pack`  | smart fetch advertise | read |
//! | `GET  info/refs?service=git-receive-pack` | smart push advertise  | write |
//! | `GET  info/refs` (no service)             | dumb advertise        | read |
//! | `GET  HEAD` / `objects/...`               | dumb object serving   | read |
//! | `POST git-upload-pack`                     | smart fetch RPC       | read |
//! | `POST git-receive-pack`                    | smart push RPC        | write |

use axum::body::{Body, Bytes};
use axum::extract::{Path, RawQuery, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use rithub_core::{Repository as RepoMeta, ReviewStore, Store};

use crate::auth::{authenticate, authorize_read, authorize_write};
use crate::state::AppState;

/// `GET /{owner}/{repo}.git/{*path}` — smart advertisements + dumb serving.
pub async fn git_get(
    State(state): State<AppState>,
    Path((owner, repo, path)): Path<(String, String, String)>,
    RawQuery(query): RawQuery,
    headers: HeaderMap,
) -> Response {
    let Some(repo_name) = repo.strip_suffix(".git") else {
        return status(StatusCode::NOT_FOUND);
    };
    let meta = match lookup(&state, &owner, repo_name).await {
        Ok(meta) => meta,
        Err(response) => return response,
    };
    let user = match authenticate(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };

    if path == "info/refs" {
        return match service_of(query.as_deref()) {
            Some("git-upload-pack") => {
                if let Err(r) = authorize_read(&state, &meta, user.as_ref()).await {
                    return r;
                }
                match state
                    .host
                    .upload_pack_advertise(&owner, repo_name, git_protocol(&headers))
                {
                    Ok(body) => smart(body, "application/x-git-upload-pack-advertisement"),
                    Err(_) => status(StatusCode::INTERNAL_SERVER_ERROR),
                }
            }
            Some("git-receive-pack") => {
                if let Err(r) = authorize_write(&state, &meta, user.as_ref()).await {
                    return r;
                }
                match state.host.receive_pack_advertise(&owner, repo_name) {
                    Ok(body) => smart(body, "application/x-git-receive-pack-advertisement"),
                    Err(_) => status(StatusCode::INTERNAL_SERVER_ERROR),
                }
            }
            // No/unknown service → dumb advertisement (read).
            _ => {
                if let Err(r) = authorize_read(&state, &meta, user.as_ref()).await {
                    return r;
                }
                match state.host.info_refs(&owner, repo_name) {
                    Ok(body) => text_plain(body.into_bytes()),
                    Err(_) => status(StatusCode::INTERNAL_SERVER_ERROR),
                }
            }
        };
    }

    // Everything else is a dumb read resource.
    if let Err(r) = authorize_read(&state, &meta, user.as_ref()).await {
        return r;
    }
    dumb_resource(&state, &owner, repo_name, &path)
}

/// `POST /{owner}/{repo}.git/{*path}` — smart RPC (`git-upload-pack` fetch,
/// `git-receive-pack` push).
pub async fn git_post(
    State(state): State<AppState>,
    Path((owner, repo, path)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(repo_name) = repo.strip_suffix(".git") else {
        return status(StatusCode::NOT_FOUND);
    };
    let meta = match lookup(&state, &owner, repo_name).await {
        Ok(meta) => meta,
        Err(response) => return response,
    };
    let user = match authenticate(&state, &headers).await {
        Ok(user) => user,
        Err(response) => return response,
    };

    match path.as_str() {
        "git-upload-pack" => {
            if let Err(r) = authorize_read(&state, &meta, user.as_ref()).await {
                return r;
            }
            match state
                .host
                .upload_pack_rpc(&owner, repo_name, &body, git_protocol(&headers))
            {
                Ok(out) => smart(out, "application/x-git-upload-pack-result"),
                Err(_) => status(StatusCode::INTERNAL_SERVER_ERROR),
            }
        }
        "git-receive-pack" => {
            if let Err(r) = authorize_write(&state, &meta, user.as_ref()).await {
                return r;
            }
            // Build the server-side push policy: which branches block direct
            // pushes, and whether this pusher (admin+) may bypass that.
            let protected = state
                .store
                .list_push_protected_branches(meta.id)
                .await
                .unwrap_or_default();
            let can_push_protected = matches!(
                crate::auth::effective_role(&state, &meta, user.as_ref()).await,
                Some(role) if role >= rithub_core::RepoRole::Admin
            );
            let policy = rithub_git::PushPolicy {
                protected_branches: protected.into_iter().collect(),
                can_push_protected,
            };
            match state.host.receive_pack(&owner, repo_name, &body, &policy) {
                Ok(outcome) => {
                    audit_push(&state, &meta, user.as_ref(), &outcome).await;
                    smart(outcome.response, "application/x-git-receive-pack-result")
                }
                // A malformed request body (bad framing) is the client's fault.
                Err(_) => status(StatusCode::BAD_REQUEST),
            }
        }
        _ => status(StatusCode::NOT_FOUND),
    }
}

/// Record a push outcome (accepted or rejected) in the audit log.
async fn audit_push(
    state: &AppState,
    meta: &RepoMeta,
    user: Option<&rithub_core::User>,
    outcome: &rithub_git::ReceivePackOutcome,
) {
    use rithub_core::{AuditStore, NewAuditEvent};
    let actor_name = user.map(|u| u.username.as_str()).unwrap_or("anonymous");
    let target = outcome.ref_updates.join(",");
    let (action, detail) = if outcome.rejected {
        ("push.rejected", outcome.reason.clone())
    } else if outcome.changed {
        ("push", None)
    } else {
        return; // nothing happened (e.g. up-to-date); don't spam the log
    };
    let _ = state
        .store
        .record_event(NewAuditEvent {
            repo_id: Some(meta.id),
            actor_id: user.map(|u| u.id),
            actor_name,
            action,
            target: Some(&target),
            detail: detail.as_deref(),
        })
        .await;

    // Deliver a signed webhook `push` event on a successful ref update.
    if action == "push" {
        crate::webhooks::dispatch(
            state,
            meta.id,
            "push",
            serde_json::json!({
                "event": "push",
                "repo": meta.name,
                "pusher": actor_name,
                "refs": outcome.ref_updates,
            }),
        );
    }
}

/// Look up repository metadata (visibility + owning org) or map a miss to 404.
async fn lookup(state: &AppState, owner: &str, repo: &str) -> Result<RepoMeta, Response> {
    state
        .store
        .get_repo(owner, repo)
        .await
        .map_err(|_| status(StatusCode::NOT_FOUND))
}

/// Serve a dumb-protocol resource: `HEAD`, `objects/info/packs`, a loose object,
/// or a pack file.
fn dumb_resource(state: &AppState, owner: &str, repo: &str, path: &str) -> Response {
    match path {
        "HEAD" => match state.host.head(owner, repo) {
            Ok(body) => text_plain(body),
            Err(_) => status(StatusCode::NOT_FOUND),
        },
        "objects/info/packs" => match state.host.objects_info_packs(owner, repo) {
            Ok(body) => text_plain(body.into_bytes()),
            Err(_) => status(StatusCode::INTERNAL_SERVER_ERROR),
        },
        "objects/info/alternates" | "objects/info/http-alternates" => {
            status(StatusCode::NOT_FOUND)
        }
        other => serve_object(state, owner, repo, other),
    }
}

fn serve_object(state: &AppState, owner: &str, repo: &str, path: &str) -> Response {
    let Some(rest) = path.strip_prefix("objects/") else {
        return status(StatusCode::NOT_FOUND);
    };
    if let Some(pack) = rest.strip_prefix("pack/") {
        return match state.host.pack_file(owner, repo, pack) {
            Ok(bytes) => {
                let ct = if pack.ends_with(".idx") {
                    "application/x-git-packed-objects-toc"
                } else {
                    "application/x-git-packed-objects"
                };
                binary(bytes, ct)
            }
            Err(rithub_git::GitHostError::NotFound(_)) => status(StatusCode::NOT_FOUND),
            Err(_) => status(StatusCode::BAD_REQUEST),
        };
    }
    let mut parts = rest.splitn(2, '/');
    match (parts.next(), parts.next()) {
        (Some(shard), Some(suffix)) => match state.host.loose_object(owner, repo, shard, suffix) {
            Ok(bytes) => binary(bytes, "application/x-git-loose-object"),
            Err(rithub_git::GitHostError::NotFound(_)) => status(StatusCode::NOT_FOUND),
            Err(_) => status(StatusCode::BAD_REQUEST),
        },
        _ => status(StatusCode::NOT_FOUND),
    }
}

/// Extract the `service` value from a raw query string (`service=git-...`).
fn service_of(query: Option<&str>) -> Option<&str> {
    query?.split('&').find_map(|pair| pair.strip_prefix("service="))
}

/// The client's `Git-Protocol` header value, if any (e.g. `version=2`).
fn git_protocol(headers: &HeaderMap) -> Option<&str> {
    headers.get("git-protocol").and_then(|v| v.to_str().ok())
}

// --- response helpers -------------------------------------------------------

fn no_cache(response: &mut Response) {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache, max-age=0, must-revalidate"),
    );
}

fn smart(bytes: Vec<u8>, content_type: &'static str) -> Response {
    binary(bytes, content_type)
}

fn text_plain(bytes: Vec<u8>) -> Response {
    let mut response = (StatusCode::OK, bytes).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    no_cache(&mut response);
    response
}

fn binary(bytes: Vec<u8>, content_type: &'static str) -> Response {
    let mut response = Response::new(Body::from(bytes));
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    no_cache(&mut response);
    response
}

fn status(code: StatusCode) -> Response {
    (code, ()).into_response()
}
