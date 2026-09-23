//! Authentication + authorization for the JSON API and the web UI.
//!
//! Every non-git surface funnels through this module so there is exactly one
//! answer to "who is calling and may they do this":
//!
//! ## Credentials (first match wins)
//! 1. `Authorization: Bearer <pat>` or `Authorization: token <pat>` — a personal
//!    access token presented on its own (the `gh`/API-client convention).
//! 2. `Authorization: Basic base64(username:pat)` — PAT or deploy-key token,
//!    identical to the git transport ([`crate::auth::authenticate`]).
//! 3. The `rithub_session` web-session cookie ([`crate::web::session_user`]).
//!
//! Web-session tokens carry the [`SESSION_SCOPE`] scope and are rejected when
//! presented as a bearer/basic credential, so a stolen session cookie cannot be
//! replayed against git or the API outside the browser session.
//!
//! ## Authorization
//! - **Repo reads** follow visibility: public repos are world-readable, private
//!   repos need ≥Read. A private repo the caller cannot read is reported as
//!   **404**, never 403, so its existence is not disclosed.
//! - **Repo writes/admin** need the effective role (org role + collaborator +
//!   team grants) plus a token scope that allows it.
//! - **Org actions** need an org role (admin to manage repos, owner to manage
//!   membership).
//! - **Site admins** (`RITHUB_ADMIN_USERS`, comma-separated usernames) bypass
//!   repo/org role checks and may manage any account.

use axum::http::{header, HeaderMap};
use rithub_core::{
    hash_token, AccessStore, Organization, RepoRole, Repository, Role, Store, User, Visibility,
};

use crate::auth::Caller;
use crate::error::AppError;
use crate::state::AppState;

/// Scope stamped on tokens minted for browser sessions.
pub const SESSION_SCOPE: &str = "web-session";

/// Whether `user` is a site administrator (`RITHUB_ADMIN_USERS`).
pub fn is_site_admin(user: &User) -> bool {
    std::env::var("RITHUB_ADMIN_USERS")
        .map(|list| {
            list.split(',')
                .map(str::trim)
                .any(|name| !name.is_empty() && name == user.username)
        })
        .unwrap_or(false)
}

/// Whether self-service sign-up is enabled (`RITHUB_OPEN_SIGNUP`, default on).
pub fn open_signup() -> bool {
    !matches!(
        std::env::var("RITHUB_OPEN_SIGNUP")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase()
            .as_str(),
        "0" | "false" | "no" | "off"
    )
}

/// Extract a bare bearer/`token` credential from the Authorization header.
fn bearer(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?.trim();
    let token = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
        .or_else(|| value.strip_prefix("token "))
        .or_else(|| value.strip_prefix("Token "))?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

/// Identify the caller. `Ok(None)` = anonymous; `Err(401)` = credentials were
/// presented but are invalid (never silently downgraded to anonymous).
pub async fn api_caller(state: &AppState, headers: &HeaderMap) -> Result<Option<Caller>, AppError> {
    if let Some(token) = bearer(headers) {
        let found = state
            .store
            .find_user_by_token(&hash_token(&token))
            .await
            .map_err(AppError::from)?;
        return match found {
            Some((_, t)) if t.scopes.iter().any(|s| s == SESSION_SCOPE) => {
                Err(AppError::unauthorized("session tokens are not valid API credentials"))
            }
            Some((user, t)) => Ok(Some(Caller::User {
                user,
                scopes: t.scopes,
            })),
            None => Err(AppError::unauthorized("invalid token")),
        };
    }
    if headers.contains_key(header::AUTHORIZATION) {
        return match crate::auth::authenticate(state, headers).await {
            Ok(Some(Caller::User { scopes, .. }))
                if scopes.iter().any(|s| s == SESSION_SCOPE) =>
            {
                Err(AppError::unauthorized("session tokens are not valid API credentials"))
            }
            Ok(caller) => Ok(caller),
            Err(_) => Err(AppError::unauthorized("invalid credentials")),
        };
    }
    Ok(crate::web::session_user(state, headers)
        .await
        .map(|user| Caller::User {
            user,
            scopes: Vec::new(),
        }))
}

/// Require an authenticated **user** (deploy keys are not users).
pub async fn require_user(state: &AppState, headers: &HeaderMap) -> Result<Caller, AppError> {
    match api_caller(state, headers).await? {
        Some(caller @ Caller::User { .. }) => Ok(caller),
        Some(Caller::DeployKey { .. }) => Err(AppError::forbidden(
            "deploy keys may only access git transport for their repository",
        )),
        None => Err(AppError::unauthorized("authentication required")),
    }
}

/// The user behind a caller known to be [`Caller::User`].
pub fn caller_user(caller: &Caller) -> &User {
    caller
        .user()
        .expect("require_user only yields Caller::User")
}

/// The user's effective role on a repo (site admins are Admin everywhere).
pub async fn user_repo_role(state: &AppState, user: &User, repo: &Repository) -> Option<RepoRole> {
    if is_site_admin(user) {
        return Some(RepoRole::Admin);
    }
    state
        .store
        .effective_role(user.id, repo.org_id, repo.id)
        .await
        .ok()
        .flatten()
}

/// Whether an (optional) user may read a repo.
pub async fn user_can_read(state: &AppState, user: Option<&User>, repo: &Repository) -> bool {
    if repo.visibility == Visibility::Public {
        return true;
    }
    match user {
        Some(u) => user_repo_role(state, u, repo).await.is_some(),
        None => false,
    }
}

/// Load a repository the caller may **read**, else 404.
pub async fn repo_for_read(
    state: &AppState,
    headers: &HeaderMap,
    org: &str,
    repo: &str,
) -> Result<(Repository, Option<Caller>), AppError> {
    let caller = api_caller(state, headers).await?;
    let meta = state.store.get_repo(org, repo).await?;
    let allowed = match &caller {
        Some(Caller::DeployKey { repo_id, .. }) => *repo_id == meta.id,
        Some(c @ Caller::User { user, .. }) => {
            c.has_scope("repo:read") && user_can_read(state, Some(user), &meta).await
        }
        None => meta.visibility == Visibility::Public,
    };
    if !allowed {
        return Err(AppError::not_found(format!("repository not found: {org}/{repo}")));
    }
    Ok((meta, caller))
}

/// Load a repository on which the caller holds at least `needed`, returning the
/// acting user. Unreadable repos are 404; readable-but-insufficient is 403.
pub async fn repo_for_role(
    state: &AppState,
    headers: &HeaderMap,
    org: &str,
    repo: &str,
    needed: RepoRole,
) -> Result<(Repository, User), AppError> {
    let caller = require_user(state, headers).await?;
    let user = caller_user(&caller).clone();
    let meta = state.store.get_repo(org, repo).await?;
    let role = user_repo_role(state, &user, &meta).await;
    let readable = meta.visibility == Visibility::Public || role.is_some();
    if !readable {
        return Err(AppError::not_found(format!("repository not found: {org}/{repo}")));
    }
    let scope = match needed {
        RepoRole::Read => "repo:read",
        RepoRole::Write => "repo:write",
        RepoRole::Admin => "admin",
    };
    if !caller.has_scope(scope) {
        return Err(AppError::forbidden(format!("token lacks the '{scope}' scope")));
    }
    match role {
        Some(r) if r >= needed => Ok((meta, user)),
        _ => Err(AppError::forbidden(format!(
            "requires {} access to {org}/{repo}",
            needed.as_str()
        ))),
    }
}

/// A repository any signed-in reader may act on (comment, open issues/PRs,
/// review): the repo must be readable by the user.
pub async fn repo_for_participant(
    state: &AppState,
    headers: &HeaderMap,
    org: &str,
    repo: &str,
) -> Result<(Repository, User), AppError> {
    let caller = require_user(state, headers).await?;
    if !caller.has_scope("repo:read") {
        return Err(AppError::forbidden("token lacks the 'repo:read' scope"));
    }
    let user = caller_user(&caller).clone();
    let meta = state.store.get_repo(org, repo).await?;
    if !user_can_read(state, Some(&user), &meta).await {
        return Err(AppError::not_found(format!("repository not found: {org}/{repo}")));
    }
    Ok((meta, user))
}

/// The user's role in an org (site admins act as owners).
pub async fn user_org_role(state: &AppState, user: &User, org: &Organization) -> Option<Role> {
    if is_site_admin(user) {
        return Some(Role::Owner);
    }
    state.store.get_membership(user.id, org.id).await.ok().flatten()
}

/// Load an org on which the caller holds at least `needed`.
pub async fn org_for_role(
    state: &AppState,
    headers: &HeaderMap,
    org: &str,
    needed: Role,
) -> Result<(Organization, User), AppError> {
    let caller = require_user(state, headers).await?;
    let scoped_ok = match needed {
        Role::Member => caller.has_scope("repo:read"),
        // Creating/managing repositories in the org.
        Role::Admin => caller.has_scope("repo:write") || caller.has_scope("org:write"),
        // Membership and org-wide administration.
        Role::Owner => caller.has_scope("org:write"),
    };
    if !scoped_ok {
        return Err(AppError::forbidden("token lacks the scope for this organization action"));
    }
    let user = caller_user(&caller).clone();
    let organization = state.store.get_org(org).await?;
    match user_org_role(state, &user, &organization).await {
        Some(role) if role >= needed => Ok((organization, user)),
        Some(_) => Err(AppError::forbidden(format!(
            "requires {} role in organization {org}",
            needed.as_str()
        ))),
        None => Err(AppError::forbidden(format!("not a member of organization {org}"))),
    }
}

/// Require that the caller *is* `username` (or a site admin), returning the
/// acting user and the target account.
pub async fn self_or_admin(
    state: &AppState,
    headers: &HeaderMap,
    username: &str,
) -> Result<(User, User), AppError> {
    let caller = require_user(state, headers).await?;
    let actor = caller_user(&caller).clone();
    if actor.username == username {
        if !caller.has_scope("user") && !caller.has_scope("admin") {
            return Err(AppError::forbidden("token lacks the 'user' scope"));
        }
        let target = actor.clone();
        return Ok((actor, target));
    }
    if is_site_admin(&actor) {
        let target = state.store.get_user(username).await?;
        return Ok((actor, target));
    }
    Err(AppError::forbidden("you may only manage your own account"))
}

/// Require a site administrator.
pub async fn require_site_admin(state: &AppState, headers: &HeaderMap) -> Result<User, AppError> {
    let caller = require_user(state, headers).await?;
    let user = caller_user(&caller).clone();
    if is_site_admin(&user) {
        Ok(user)
    } else {
        Err(AppError::forbidden("site administrator required"))
    }
}
