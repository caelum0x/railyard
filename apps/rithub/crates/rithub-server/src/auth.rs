//! Authentication + authorization for the git HTTP endpoints.
//!
//! ## AuthN
//! HTTP Basic `username:secret` is tried against **two** credential stores:
//!
//! 1. **Personal access tokens (PATs)** — hashed secret looked up in `tokens`
//!    (must not be revoked or expired); username must match the token owner.
//! 2. **Deploy-key bearer tokens** — hashed secret looked up in `deploy_keys`;
//!    no username check (use `x-deploy-key` by convention). Access is scoped to
//!    the specific repository the key was created for.
//!
//! ## AuthZ
//! Resolved from the fine-grained **effective role** (org role + per-repo
//! collaborator + team grants) for PAT callers, or from the deploy key's own
//! `read_only` flag for deploy-key callers. Read (clone/fetch) is anonymous for
//! **public** repos and requires ≥Read for **private**; write (push) requires
//! **≥Write**. There is no bypass.
//!
//! ## Scope enforcement
//! PATs with a non-empty scope list are checked: a write operation requires the
//! token to have at least one of `["repo:write", "admin"]`; a read operation
//! requires at least one of `["repo:read", "repo:write", "admin"]`. A token
//! with an empty scope list is treated as unrestricted (legacy behaviour).

use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine;
use rithub_core::{AccessStore, DeployKeyStore, Repository, RepoRole, Store, User, Visibility};
use uuid::Uuid;

use crate::state::AppState;

// ---------------------------------------------------------------------------
// Caller identity
// ---------------------------------------------------------------------------

/// Who is making the request, and with what privileges.
#[derive(Clone, Debug)]
pub enum Caller {
    /// Authenticated via a personal access token.
    User {
        user: User,
        /// Scopes granted by this token. Empty = unrestricted (legacy tokens).
        scopes: Vec<String>,
    },
    /// Authenticated via a deploy-key bearer token (HTTP) or SSH public key.
    DeployKey {
        /// The repository this deploy key is restricted to.
        repo_id: Uuid,
        /// If `true` the key only allows reads; pushes are rejected.
        read_only: bool,
    },
}

impl Caller {
    /// Extract the user if this is a PAT caller.
    pub fn user(&self) -> Option<&User> {
        match self {
            Caller::User { user, .. } => Some(user),
            Caller::DeployKey { .. } => None,
        }
    }

    /// Whether this token/key grants the named scope (or is unrestricted).
    ///
    /// For deploy keys `scope` is always treated as granted — the `read_only`
    /// flag is checked separately in [`authorize_read`] / [`authorize_write`].
    pub fn has_scope(&self, scope: &str) -> bool {
        match self {
            Caller::DeployKey { .. } => true,
            Caller::User { scopes, .. } => {
                scopes.is_empty()
                    || scopes.iter().any(|s| s == scope || s == "admin")
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Authenticate
// ---------------------------------------------------------------------------

/// Authenticate the request. Returns:
/// - `Ok(None)` — no credentials presented (anonymous),
/// - `Ok(Some(caller))` — valid PAT or deploy-key token,
/// - `Err(response)` (401) — credentials were present but invalid.
pub async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<Option<Caller>, Response> {
    let Some((username, secret)) = parse_basic(headers) else {
        return Ok(None);
    };

    let hash = rithub_core::hash_token(&secret);

    // 1. Try PAT lookup (must not be revoked or expired — enforced in SQL).
    if let Some((user, token)) = state
        .store
        .find_user_by_token(&hash)
        .await
        .map_err(|_| internal())?
    {
        if user.username != username {
            return Err(challenge("invalid credentials"));
        }
        return Ok(Some(Caller::User {
            user,
            scopes: token.scopes,
        }));
    }

    // 2. Try deploy-key bearer token lookup (repo-scoped, no username check).
    if let Some(dk) = state
        .store
        .find_deploy_key_by_token(&hash)
        .await
        .map_err(|_| internal())?
    {
        return Ok(Some(Caller::DeployKey {
            repo_id: dk.repo_id,
            read_only: dk.read_only,
        }));
    }

    // Credentials were presented but matched nothing.
    Err(challenge("invalid credentials"))
}

// ---------------------------------------------------------------------------
// Authorize
// ---------------------------------------------------------------------------

/// The user's effective repo role, or `None` if no grant (or not a user caller).
pub async fn effective_role(
    state: &AppState,
    repo: &Repository,
    caller: Option<&Caller>,
) -> Option<RepoRole> {
    let user = caller.and_then(Caller::user)?;
    state
        .store
        .effective_role(user.id, repo.org_id, repo.id)
        .await
        .ok()
        .flatten()
}

/// Authorize a read (clone/fetch).
/// - Public repos: anonymous OK.
/// - Private repos: ≥Read role required.
/// - Deploy keys: must be scoped to this repo.
/// - Scope check: token must hold `repo:read`, `repo:write`, or `admin` (or be
///   unrestricted).
pub async fn authorize_read(
    state: &AppState,
    repo: &Repository,
    caller: Option<&Caller>,
) -> Result<(), Response> {
    if let Some(Caller::DeployKey { repo_id, .. }) = caller {
        if *repo_id != repo.id {
            return Err(forbidden());
        }
        return Ok(());
    }

    // Scope check for PAT callers.
    if let Some(caller) = caller {
        if !caller.has_scope("repo:read") {
            return Err(forbidden());
        }
    }

    if repo.visibility == Visibility::Public {
        return Ok(());
    }
    require_role(state, repo, caller, RepoRole::Read).await
}

/// Authorize a write (push): ≥Write role for users, non-read-only deploy keys.
pub async fn authorize_write(
    state: &AppState,
    repo: &Repository,
    caller: Option<&Caller>,
) -> Result<(), Response> {
    match caller {
        Some(Caller::DeployKey { repo_id, read_only }) => {
            if *repo_id != repo.id {
                return Err(forbidden());
            }
            if *read_only {
                return Err(forbidden());
            }
            return Ok(());
        }
        Some(user_caller) => {
            if !user_caller.has_scope("repo:write") {
                return Err(forbidden());
            }
        }
        None => {}
    }
    require_role(state, repo, caller, RepoRole::Write).await
}

/// Require that `caller`'s effective role on the repo is at least `needed`.
async fn require_role(
    state: &AppState,
    repo: &Repository,
    caller: Option<&Caller>,
    needed: RepoRole,
) -> Result<(), Response> {
    if caller.is_none() {
        return Err(challenge("authentication required"));
    }
    match effective_role(state, repo, caller).await {
        Some(role) if role >= needed => Ok(()),
        _ => Err(forbidden()),
    }
}

// ---------------------------------------------------------------------------
// HTTP helpers
// ---------------------------------------------------------------------------

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
