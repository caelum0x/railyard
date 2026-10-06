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

use crate::auth::{authenticate, authorize_read, authorize_write, Caller};
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
    let caller = match authenticate(&state, &headers).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };

    if path == "info/refs" {
        return match service_of(query.as_deref()) {
            Some("git-upload-pack") => {
                if let Err(r) = authorize_read(&state, &meta, caller.as_ref()).await {
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
                if let Err(r) = authorize_write(&state, &meta, caller.as_ref()).await {
                    return r;
                }
                match state.host.receive_pack_advertise(&owner, repo_name) {
                    Ok(body) => smart(body, "application/x-git-receive-pack-advertisement"),
                    Err(_) => status(StatusCode::INTERNAL_SERVER_ERROR),
                }
            }
            // No/unknown service → dumb advertisement (read).
            _ => {
                if let Err(r) = authorize_read(&state, &meta, caller.as_ref()).await {
                    return r;
                }
                match state.host.info_refs(&owner, repo_name) {
                    Ok(body) => text_plain(body.into_bytes()),
                    Err(_) => status(StatusCode::INTERNAL_SERVER_ERROR),
                }
            }
        };
    }

    if let Some(oid) = path.strip_prefix("info/lfs/objects/") {
        return crate::lfs::download(&state, &meta, caller.as_ref(), oid).await;
    }

    // Everything else is a dumb read resource.
    if let Err(r) = authorize_read(&state, &meta, caller.as_ref()).await {
        return r;
    }
    dumb_resource(&state, &owner, repo_name, &path)
}

/// `PUT /{owner}/{repo}.git/info/lfs/objects/{oid}` — an LFS upload.
pub async fn git_put(
    State(state): State<AppState>,
    Path((owner, repo, path)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(repo_name) = repo.strip_suffix(".git") else {
        return status(StatusCode::NOT_FOUND);
    };
    let Some(oid) = path.strip_prefix("info/lfs/objects/") else {
        return status(StatusCode::METHOD_NOT_ALLOWED);
    };
    let meta = match lookup(&state, &owner, repo_name).await {
        Ok(meta) => meta,
        Err(response) => return response,
    };
    let caller = match authenticate(&state, &headers).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };
    crate::lfs::upload(&state, &meta, caller.as_ref(), oid, &body).await
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
    let caller = match authenticate(&state, &headers).await {
        Ok(caller) => caller,
        Err(response) => return response,
    };

    match path.as_str() {
        "info/lfs/objects/batch" => {
            crate::lfs::batch(&state, &owner, repo_name, &meta, caller.as_ref(), &headers, &body).await
        }
        "git-upload-pack" => {
            if let Err(r) = authorize_read(&state, &meta, caller.as_ref()).await {
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
            if let Err(r) = authorize_write(&state, &meta, caller.as_ref()).await {
                return r;
            }
            // Build the server-side push policy: which branches block direct
            // pushes, and whether this pusher (admin+) may bypass that.
            // Deploy keys never bypass protected branches.
            let protected = state
                .store
                .list_push_protected_branches(meta.id)
                .await
                .unwrap_or_default();
            let can_push_protected = matches!(
                crate::auth::effective_role(&state, &meta, caller.as_ref()).await,
                Some(role) if role >= rithub_core::RepoRole::Admin
            );
            let policy = rithub_git::PushPolicy {
                protected_branches: protected.into_iter().collect(),
                can_push_protected,
            };
            match state.host.receive_pack(&owner, repo_name, &body, &policy) {
                Ok(outcome) => {
                    let report = PushReport {
                        rejected: outcome.rejected,
                        changed: outcome.changed,
                        reason: outcome.reason.clone(),
                        // Over HTTP the new tips are resolved by the CI worker.
                        ref_updates: outcome
                            .ref_updates
                            .iter()
                            .map(|r| RefUpdate { name: r.clone(), new_sha: None, deleted: false })
                            .collect(),
                    };
                    post_push(&state, &owner, &meta, caller.as_ref().and_then(Caller::user), &report)
                        .await;
                    smart(outcome.response, "application/x-git-receive-pack-result")
                }
                // A malformed request body (bad framing) is the client's fault.
                Err(_) => status(StatusCode::BAD_REQUEST),
            }
        }
        _ => status(StatusCode::NOT_FOUND),
    }
}

/// One ref touched by a push.
pub(crate) struct RefUpdate {
    /// Full ref name (`refs/heads/main`).
    pub name: String,
    /// The new tip when known (SSH diffs refs); `None` lets the CI worker
    /// resolve it (HTTP).
    pub new_sha: Option<String>,
    /// The ref was deleted by this push.
    pub deleted: bool,
}

/// Transport-neutral result of a receive-pack, shared by HTTP and SSH.
pub(crate) struct PushReport {
    pub rejected: bool,
    pub changed: bool,
    pub reason: Option<String>,
    pub ref_updates: Vec<RefUpdate>,
}

/// Everything that follows a push, identical for the HTTP and SSH transports:
/// audit (`push` / `push.rejected`), the signed `push` webhook, Rago metering,
/// CI dispatch per updated ref, and a search re-index of the repo's git content.
pub(crate) async fn post_push(
    state: &AppState,
    owner: &str,
    meta: &RepoMeta,
    user: Option<&rithub_core::User>,
    report: &PushReport,
) {
    let actor_name = user.map(|u| u.username.as_str()).unwrap_or("deploy-key");
    let ref_names: Vec<String> = report.ref_updates.iter().map(|r| r.name.clone()).collect();
    let target = ref_names.join(",");
    let (action, detail) = if report.rejected {
        ("push.rejected", report.reason.clone())
    } else if report.changed {
        ("push", None)
    } else {
        return; // nothing happened (e.g. up-to-date); don't spam the log
    };
    // Audit append via outbox (non-blocking, durable).
    crate::audit::record(
        state,
        Some(meta.id),
        user.map(|u| u.id),
        actor_name,
        action,
        Some(&target),
        detail.as_deref(),
    )
    .await;

    if action != "push" {
        return;
    }

    // Enqueue a signed webhook `push` event job on a successful ref update.
    {
        use rithub_core::{OutboxStore, KIND_WEBHOOK_DELIVERY};
        let state2 = state.clone();
        let repo_id = meta.id;
        let repo_name = meta.name.clone();
        let pusher = actor_name.to_string();
        let refs = ref_names.clone();
        tokio::spawn(async move {
            let payload = serde_json::json!({
                "repo_id": repo_id,
                "event": "push",
                "event_payload": {
                    "event": "push",
                    "repo": repo_name,
                    "pusher": pusher,
                    "refs": refs,
                },
            });
            state2.store.enqueue_job(KIND_WEBHOOK_DELIVERY, payload)
                .await
                .inspect_err(|e| tracing::warn!(error = %e, "failed to enqueue push webhook job"))
                .ok();
        });
    }

    // Meter the accepted push against the org's Rago subscription (no-op
    // when billing is not configured). Keyed by the updated refs + time so
    // a retried delivery of the same event dedupes.
    crate::billing::meter(
        owner,
        crate::billing::METRIC_CI_PUSH,
        format!(
            "push|{}|{}|{}",
            meta.id,
            target,
            time::OffsetDateTime::now_utc().unix_timestamp()
        ),
        serde_json::json!({
            "repo": format!("{owner}/{}", meta.name),
            "refs": ref_names.len(),
        }),
    );

    // Dispatch CI for each updated ref. When the new tip is unknown the worker
    // resolves `ref` (deleted refs simply fail to resolve and are skipped).
    for update in report.ref_updates.iter().filter(|u| !u.deleted) {
        crate::ci::dispatch_check_runs(
            state,
            meta.id,
            owner,
            &meta.name,
            update.new_sha.as_deref().unwrap_or(""),
            &update.name,
            "push",
        );
    }

    // Refresh the search index for this repo's git content. Detached +
    // best-effort so it never delays or fails the client's push.
    let st = state.clone();
    let owner_s = owner.to_string();
    let meta_c = meta.clone();
    tokio::spawn(async move {
        crate::search_index::index_repo_git(&st, &owner_s, &meta_c).await;
    });
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
