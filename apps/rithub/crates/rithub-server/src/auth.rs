//! Authentication + authorization for the git HTTP endpoints.
//!
//! - **AuthN:** HTTP Basic `username:token`, where `token` is a personal access
//!   token. We hash the presented token and look it up (constant work regardless
//!   of validity path); the username must match the token's owner.
//! - **AuthZ:** resolved from the fine-grained **effective role** (org role +
//!   per-repo collaborator + team grants). Read (clone/fetch) is anonymous for
//!   **public** repos and requires ≥Read for **private**; write (push) requires
//!   **≥Write**; admin actions require **≥Admin**. There is no bypass — every git
//!   request is gated before any object or ref is touched.

use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use rithub_core::{AccessStore, hash_token, Repository, RepoRole, Store, User, Visibility};

use crate::state::AppState;

/// The user's effective role on a repo, or `None` if they have no grant.
pub async fn effective_role(
    state: &AppState,
    repo: &Repository,
    user: Option<&User>,
) -> Option<RepoRole> {
    let user = user?;
    state
        .store
        .effective_role(user.id, repo.org_id, repo.id)
        .await
        .ok()
        .flatten()
}

/// Authenticate the request. Returns:
/// - `Ok(None)` when no credentials are presented (anonymous),
/// - `Ok(Some(user))` when a valid token maps to the presented username,
/// - `Err(response)` (401) when credentials are present but invalid.
pub async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<User>, Response> {
    let Some((username, secret)) = parse_basic(headers) else {
        return Ok(None);
    };
    match state.store.find_user_by_token(&hash_token(&secret)).await {
        Ok(Some(user)) if user.username == username => Ok(Some(user)),
        Ok(_) => Err(challenge("invalid credentials")),
        Err(_) => Err(internal()),
    }
}

/// Authorize a read (clone/fetch). Public repos are world-readable; private
/// repos require an authenticated user with ≥Read.
pub async fn authorize_read(
    state: &AppState,
    repo: &Repository,
    user: Option<&User>,
) -> Result<(), Response> {
    if repo.visibility == Visibility::Public {
        return Ok(());
    }
    require_role(state, repo, user, RepoRole::Read).await
}

/// Authorize a write (push): requires an authenticated user with ≥Write.
pub async fn authorize_write(
    state: &AppState,
    repo: &Repository,
    user: Option<&User>,
) -> Result<(), Response> {
    require_role(state, repo, user, RepoRole::Write).await
}

/// Require that `user`'s effective role on the repo is at least `needed`.
async fn require_role(
    state: &AppState,
    repo: &Repository,
    user: Option<&User>,
    needed: RepoRole,
) -> Result<(), Response> {
    if user.is_none() {
        return Err(challenge("authentication required"));
    }
    match effective_role(state, repo, user).await {
        Some(role) if role >= needed => Ok(()),
        _ => Err(forbidden()),
    }
}

/// Parse an HTTP Basic `Authorization` header into `(username, secret)`.
fn parse_basic(headers: &HeaderMap) -> Option<(String, String)> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let b64 = value.strip_prefix("Basic ").or_else(|| value.strip_prefix("basic "))?;
    let decoded = base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (user, secret) = text.split_once(':')?;
    Some((user.to_string(), secret.to_string()))
}

/// A 401 that prompts git to supply Basic credentials.
fn challenge(message: &str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"rithub\"")],
        message.to_string(),
    )
        .into_response()
}

fn forbidden() -> Response {
    (StatusCode::FORBIDDEN, "forbidden").into_response()
}

fn internal() -> Response {
    (StatusCode::INTERNAL_SERVER_ERROR, "internal server error").into_response()
}
