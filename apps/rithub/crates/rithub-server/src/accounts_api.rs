//! Account JSON API: sign-up/provisioning, the current user, personal access
//! tokens, and SSH keys. Self-service (a user manages their own credentials) or
//! site-admin. Re-exported through [`crate::api`].

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use rithub_core::{
    generate_token, hash_token, CoreError, DeployKeyStore, DirectoryStore, NewOrganization, Role,
    SshKeyRecord, SshKeyStore, Store, Token, User,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::api::validate_namespace;
use crate::auth::Caller;
use crate::authz;
use crate::error::AppError;
use crate::state::AppState;

// --- accounts ---------------------------------------------------------------

/// Minimal email sanity check (the address is used for notifications).
fn validate_email(email: &str) -> Result<(), AppError> {
    let e = email.trim();
    let ok = e.len() <= 254
        && e.split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'))
        && !e.contains(char::is_whitespace);
    if ok {
        Ok(())
    } else {
        Err(AppError::bad_request("a valid email address is required"))
    }
}

/// Create a user plus their **personal organization** (same name, user as
/// owner), so every account has a namespace to create and fork repos into.
pub async fn provision_user(state: &AppState, username: &str, email: &str) -> Result<User, AppError> {
    validate_namespace("user", username)?;
    validate_email(email)?;
    let user = state.store.create_user(username, email.trim()).await?;
    ensure_personal_org(state, &user).await;
    crate::audit::record(state, None, Some(user.id), &user.username, "user.created", None, None)
        .await;
    Ok(user)
}

/// Best-effort: create the user's personal org if the name is free.
pub async fn ensure_personal_org(state: &AppState, user: &User) {
    let org = match state
        .store
        .create_org(&NewOrganization {
            name: user.username.clone(),
            display_name: Some(user.username.clone()),
        })
        .await
    {
        Ok(org) => org,
        Err(CoreError::Conflict { .. }) => return, // name taken by an existing org
        Err(e) => {
            tracing::warn!(error = %e, user = %user.username, "personal org creation failed");
            return;
        }
    };
    if let Err(e) = state.store.upsert_membership(user.id, org.id, Role::Owner).await {
        tracing::warn!(error = %e, user = %user.username, "personal org owner grant failed");
    }
}

/// Mint a PAT for `user` and return the plaintext exactly once.
pub async fn mint_token(
    state: &AppState,
    user: &User,
    name: &str,
    scopes: &[String],
    expires_at: Option<OffsetDateTime>,
) -> Result<(String, Token), AppError> {
    let name = name.trim();
    if name.is_empty() || name.len() > 100 {
        return Err(AppError::bad_request("token name must be 1-100 characters"));
    }
    if scopes.iter().any(|s| s == authz::SESSION_SCOPE) {
        return Err(AppError::bad_request("that scope is reserved"));
    }
    if expires_at.is_some_and(|t| t <= OffsetDateTime::now_utc()) {
        return Err(AppError::bad_request("expiry must be in the future"));
    }
    let token = generate_token();
    let record = state
        .store
        .create_token(user.id, name, &hash_token(&token), scopes, expires_at)
        .await?;
    crate::audit::record(state, None, Some(user.id), &user.username, "token.created", Some(name), None)
        .await;
    Ok((token, record))
}

/// Request body for creating a user.
#[derive(Debug, Deserialize)]
pub struct NewUser {
    pub username: String,
    pub email: String,
}

/// Response for a new account: the user plus a first PAT (shown once).
#[derive(Debug, Serialize)]
pub struct NewUserResponse {
    #[serde(flatten)]
    pub user: User,
    /// A full-access personal access token for the new account. Shown once.
    pub token: String,
}

/// `POST /api/users` — create an account. Open to anyone when self-service
/// sign-up is enabled (`RITHUB_OPEN_SIGNUP`, default on); otherwise site
/// admins only. Returns the account's first personal access token.
pub async fn create_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<NewUser>,
) -> Result<Json<NewUserResponse>, AppError> {
    if !authz::open_signup() {
        authz::require_site_admin(&state, &headers).await?;
    }
    let user = provision_user(&state, input.username.trim(), &input.email).await?;
    let (token, _) = mint_token(&state, &user, "initial", &[], None).await?;
    Ok(Json(NewUserResponse { user, token }))
}

/// `GET /api/user` — the authenticated account.
pub async fn current_user(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let caller = authz::require_user(&state, &headers).await?;
    let user = authz::caller_user(&caller).clone();
    let orgs = state.store.list_user_orgs(user.id).await?;
    Ok(Json(serde_json::json!({
        "user": user,
        "site_admin": authz::is_site_admin(&user),
        "orgs": orgs.iter().map(|(o, r)| serde_json::json!({"name": o.name, "role": r.as_str()})).collect::<Vec<_>>(),
    })))
}

/// Request body for minting a personal access token.
#[derive(Debug, Deserialize)]
pub struct NewToken {
    /// Target account. Defaults to the caller; another account needs site admin.
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default = "default_token_name")]
    pub name: String,
    /// Optional list of scopes. Omit or pass `[]` for a full-access token.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Optional RFC-3339 expiry timestamp.
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub expires_at: Option<OffsetDateTime>,
}

fn default_token_name() -> String {
    "default".to_string()
}

/// Response for a newly minted token — the plaintext `token` is shown once.
#[derive(Debug, Serialize)]
pub struct TokenResponse {
    pub id: Uuid,
    pub token: String,
    pub name: String,
    pub username: String,
    pub scopes: Vec<String>,
}

/// `POST /api/tokens` — mint a personal access token for the caller (or, for a
/// site admin, for `username`). The plaintext is returned exactly once.
pub async fn create_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<NewToken>,
) -> Result<Json<TokenResponse>, AppError> {
    let caller = authz::require_user(&state, &headers).await?;
    let actor = authz::caller_user(&caller).clone();
    let target_name = input.username.clone().unwrap_or_else(|| actor.username.clone());
    let (_, target) = authz::self_or_admin(&state, &headers, &target_name).await?;
    // A restricted token may never mint a broader one.
    if let Caller::User { scopes, .. } = &caller {
        if !scopes.is_empty() && !scopes.iter().any(|s| s == "admin") {
            let widening = input.scopes.is_empty() || input.scopes.iter().any(|s| !scopes.contains(s));
            if widening {
                return Err(AppError::forbidden("a scoped token cannot mint a broader token"));
            }
        }
    }
    let (token, record) = mint_token(&state, &target, &input.name, &input.scopes, input.expires_at).await?;
    Ok(Json(TokenResponse {
        id: record.id,
        token,
        name: record.name,
        username: target.username,
        scopes: record.scopes,
    }))
}

/// `GET /api/users/{username}/tokens` — list tokens (no hashes; self or admin).
pub async fn list_tokens(
    State(state): State<AppState>,
    Path(username): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<Token>>, AppError> {
    let (_, target) = authz::self_or_admin(&state, &headers, &username).await?;
    Ok(Json(state.store.list_tokens(target.id).await?))
}

/// `DELETE /api/users/{username}/tokens/{token_id}` — revoke a token.
pub async fn revoke_token(
    State(state): State<AppState>,
    Path((username, token_id)): Path<(String, Uuid)>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let (actor, target) = authz::self_or_admin(&state, &headers, &username).await?;
    state.store.revoke_token(target.id, token_id).await?;
    crate::audit::record(
        &state,
        None,
        Some(actor.id),
        &actor.username,
        "token.revoked",
        Some(&token_id.to_string()),
        Some(&format!("user={}", target.username)),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Request body for registering an SSH key.
#[derive(Debug, Deserialize)]
pub struct NewSshKey {
    /// A full `authorized_keys` line, e.g. `ssh-ed25519 AAAA... comment`.
    pub key: String,
    #[serde(default)]
    pub title: Option<String>,
}

/// Parse an `authorized_keys` line into its base64 key blob.
pub fn parse_public_key(line: &str) -> Result<String, AppError> {
    const ALGOS: &[&str] = &[
        "ssh-ed25519", "ssh-rsa", "ecdsa-sha2-nistp256", "ecdsa-sha2-nistp384",
        "ecdsa-sha2-nistp521", "sk-ssh-ed25519@openssh.com", "sk-ecdsa-sha2-nistp256@openssh.com",
    ];
    let mut parts = line.split_whitespace();
    let algo = parts.next().unwrap_or_default();
    let blob = parts
        .next()
        .ok_or_else(|| AppError::bad_request("malformed public key line"))?;
    if !ALGOS.contains(&algo) {
        return Err(AppError::bad_request(format!("unsupported key type '{algo}'")));
    }
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(blob)
        .map_err(|_| AppError::bad_request("public key is not valid base64"))?;
    Ok(blob.to_string())
}

/// Register an SSH key for `user` (shared with the web settings page).
pub async fn register_ssh_key(
    state: &AppState,
    user: &User,
    line: &str,
    title: Option<&str>,
) -> Result<String, AppError> {
    let blob = parse_public_key(line)?;
    if state.store.find_user_by_ssh_key(&blob).await?.is_some()
        || state.store.find_deploy_key_by_blob(&blob).await?.is_some()
    {
        return Err(AppError::conflict("that key is already registered"));
    }
    let title = title.map(str::trim).filter(|t| !t.is_empty()).unwrap_or("key");
    state.store.add_ssh_key(user.id, title, &blob).await?;
    crate::audit::record(state, None, Some(user.id), &user.username, "ssh_key.added", Some(title), None)
        .await;
    Ok(title.to_string())
}

/// `POST /api/users/{username}/keys` — register an SSH public key (self/admin).
pub async fn add_ssh_key(
    State(state): State<AppState>,
    Path(username): Path<String>,
    headers: HeaderMap,
    Json(input): Json<NewSshKey>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (_, target) = authz::self_or_admin(&state, &headers, &username).await?;
    let title = register_ssh_key(&state, &target, &input.key, input.title.as_deref()).await?;
    Ok(Json(serde_json::json!({ "username": target.username, "title": title })))
}

/// `GET /api/users/{username}/keys` — list SSH keys (self/admin).
pub async fn list_ssh_keys(
    State(state): State<AppState>,
    Path(username): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<SshKeyRecord>>, AppError> {
    let (_, target) = authz::self_or_admin(&state, &headers, &username).await?;
    Ok(Json(state.store.list_ssh_key_records(target.id).await?))
}

/// `DELETE /api/users/{username}/keys/{key_id}` — remove an SSH key (self/admin).
pub async fn delete_ssh_key(
    State(state): State<AppState>,
    Path((username, key_id)): Path<(String, Uuid)>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let (actor, target) = authz::self_or_admin(&state, &headers, &username).await?;
    state.store.delete_ssh_key(target.id, key_id).await?;
    crate::audit::record(
        &state,
        None,
        Some(actor.id),
        &actor.username,
        "ssh_key.deleted",
        Some(&key_id.to_string()),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

