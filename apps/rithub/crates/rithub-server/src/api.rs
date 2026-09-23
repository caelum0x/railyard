//! The forge JSON API: organizations and repositories. Real handlers backed by
//! Postgres (via `rithub-core`) and the on-disk git host (via `rithub-git`).
//!
//! Creating a repository is the load-bearing action: it validates the name,
//! initializes a **bare** git repo on disk with `rit`, and records the metadata
//! row — so a freshly-created repo is immediately hostable over git HTTP.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use rithub_core::{
    generate_deploy_token, generate_token, hash_token, validate_name, DeployKey, DeployKeyStore,
    Membership, NewOrganization, NewRepository, Organization, Repository, Role, SshKeyStore,
    Store, Token, UpdateRepository, User,
};
use rithub_git::SeedFile;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;

/// `POST /api/orgs` — create an organization.
pub async fn create_org(
    State(state): State<AppState>,
    Json(input): Json<NewOrganization>,
) -> Result<Json<Organization>, AppError> {
    validate_name("organization", &input.name)?;
    let org = state.store.create_org(&input).await?;
    Ok(Json(org))
}

/// `GET /api/orgs` — list organizations.
pub async fn list_orgs(
    State(state): State<AppState>,
) -> Result<Json<Vec<Organization>>, AppError> {
    Ok(Json(state.store.list_orgs().await?))
}

/// `GET /api/orgs/{org}` — fetch one organization.
pub async fn get_org(
    State(state): State<AppState>,
    Path(org): Path<String>,
) -> Result<Json<Organization>, AppError> {
    Ok(Json(state.store.get_org(&org).await?))
}

/// `POST /api/orgs/{org}/repos` — create a repository (init bare on disk).
pub async fn create_repo(
    State(state): State<AppState>,
    Path(org_name): Path<String>,
    Json(input): Json<NewRepository>,
) -> Result<Json<Repository>, AppError> {
    validate_name("repository", &input.name)?;
    let org = state.store.get_org(&org_name).await?;
    let default_branch = input.default_branch.as_deref().unwrap_or("main");

    // Initialize the bare repo on disk first (idempotent), then record metadata.
    let path = state
        .host
        .init_bare(&org.name, &input.name, default_branch)?;
    let disk_path = path.to_string_lossy().into_owned();

    let repo = state.store.create_repo(&org, &input, &disk_path).await?;
    crate::search_index::index_repo(&state, &org.name, &repo).await;
    Ok(Json(repo))
}

/// `GET /api/orgs/{org}/repos` — list repositories in an organization.
pub async fn list_repos(
    State(state): State<AppState>,
    Path(org_name): Path<String>,
) -> Result<Json<Vec<Repository>>, AppError> {
    let org = state.store.get_org(&org_name).await?;
    Ok(Json(state.store.list_repos(&org).await?))
}

/// `GET /api/orgs/{org}/repos/{repo}` — repository metadata.
pub async fn get_repo(
    State(state): State<AppState>,
    Path((org_name, repo_name)): Path<(String, String)>,
) -> Result<Json<Repository>, AppError> {
    Ok(Json(state.store.get_repo(&org_name, &repo_name).await?))
}

/// `PATCH /api/orgs/{org}/repos/{repo}` — update mutable repository fields.
///
/// All body fields are optional. Only provided fields are changed; absent fields
/// keep their current values. Renaming a repo updates the Postgres row's `name`
/// column — the bare repo on disk is identified by `disk_path`, which never
/// changes, so the git data is unaffected.
pub async fn update_repo(
    State(state): State<AppState>,
    Path((org_name, repo_name)): Path<(String, String)>,
    Json(input): Json<UpdateRepository>,
) -> Result<Json<Repository>, AppError> {
    // Validate the new name if one was provided.
    if let Some(ref new_name) = input.name {
        validate_name("repository", new_name)?;
    }
    let repo = state.store.get_repo(&org_name, &repo_name).await?;
    let updated = state.store.update_repo(repo.id, &input).await?;
    crate::search_index::index_repo(&state, &org_name, &updated).await;
    Ok(Json(updated))
}

/// Request body for seeding a commit.
#[derive(Debug, Deserialize)]
pub struct SeedRequest {
    /// Files to place at the repository root.
    pub files: Vec<SeedFileInput>,
    /// Commit message.
    pub message: String,
    /// Optional branch (defaults to the repo's default branch).
    #[serde(default)]
    pub branch: Option<String>,
    /// Optional author name (defaults to a generic forge identity).
    #[serde(default)]
    pub author_name: Option<String>,
    /// Optional author email.
    #[serde(default)]
    pub author_email: Option<String>,
}

/// A single file to seed (text content).
#[derive(Debug, Deserialize)]
pub struct SeedFileInput {
    pub name: String,
    pub content: String,
    #[serde(default)]
    pub executable: bool,
}

/// Response for a successful seed.
#[derive(Debug, Serialize)]
pub struct SeedResponse {
    pub commit: String,
    pub branch: String,
}

/// `POST /api/orgs/{org}/repos/{repo}/seed` — write a commit into the repo via
/// `rit`. This is a real convenience over the git-hosting layer (not a stub):
/// it lets the API place content that a subsequent `git clone` will retrieve.
pub async fn seed_repo(
    State(state): State<AppState>,
    Path((org_name, repo_name)): Path<(String, String)>,
    Json(input): Json<SeedRequest>,
) -> Result<Json<SeedResponse>, AppError> {
    // Confirm the repository exists and learn its default branch.
    let repo = state.store.get_repo(&org_name, &repo_name).await?;
    if input.files.is_empty() {
        return Err(AppError::bad_request("seed requires at least one file"));
    }
    let branch = input.branch.unwrap_or_else(|| repo.default_branch.clone());
    let files: Vec<SeedFile> = input
        .files
        .into_iter()
        .map(|f| SeedFile {
            name: f.name,
            content: f.content.into_bytes(),
            executable: f.executable,
        })
        .collect();

    let commit = state.host.seed_commit(
        &org_name,
        &repo_name,
        &branch,
        &files,
        &input.message,
        input.author_name.as_deref().unwrap_or("rithub"),
        input.author_email.as_deref().unwrap_or("rithub@localhost"),
    )?;

    // Index the newly-seeded git content (commits + text blobs), best-effort.
    crate::search_index::index_repo_git(&state, &org_name, &repo).await;

    Ok(Json(SeedResponse { commit, branch }))
}

// --- users, tokens, memberships (auth plumbing) -----------------------------

/// Request body for creating a user.
#[derive(Debug, Deserialize)]
pub struct NewUser {
    pub username: String,
    pub email: String,
}

/// `POST /api/users` — create a user account.
pub async fn create_user(
    State(state): State<AppState>,
    Json(input): Json<NewUser>,
) -> Result<Json<User>, AppError> {
    validate_name("user", &input.username)?;
    Ok(Json(state.store.create_user(&input.username, &input.email).await?))
}

/// Request body for minting a personal access token.
#[derive(Debug, Deserialize)]
pub struct NewToken {
    pub username: String,
    #[serde(default = "default_token_name")]
    pub name: String,
    /// Optional list of scopes. Omit or pass `[]` for a full-access token.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Optional RFC-3339 expiry timestamp.
    #[serde(
        default,
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    pub expires_at: Option<OffsetDateTime>,
}

fn default_token_name() -> String {
    "default".to_string()
}

/// Response for a newly minted token — the plaintext `token` is shown once.
#[derive(Debug, Serialize)]
pub struct TokenResponse {
    pub token: String,
    pub name: String,
    pub username: String,
    pub scopes: Vec<String>,
}

/// `POST /api/tokens` — mint a personal access token for a user. The plaintext
/// token is returned exactly once; only its hash is stored.
pub async fn create_token(
    State(state): State<AppState>,
    Json(input): Json<NewToken>,
) -> Result<Json<TokenResponse>, AppError> {
    let user = state.store.get_user(&input.username).await?;
    let token = generate_token();
    state
        .store
        .create_token(
            user.id,
            &input.name,
            &hash_token(&token),
            &input.scopes,
            input.expires_at,
        )
        .await?;
    crate::audit::record(
        &state,
        None,
        Some(user.id),
        &user.username,
        "token.created",
        Some(&input.name),
        None,
    )
    .await;
    Ok(Json(TokenResponse {
        token,
        name: input.name,
        username: user.username,
        scopes: input.scopes,
    }))
}

/// `GET /api/users/{username}/tokens` — list a user's tokens (no hashes).
pub async fn list_tokens(
    State(state): State<AppState>,
    Path(username): Path<String>,
) -> Result<Json<Vec<Token>>, AppError> {
    let user = state.store.get_user(&username).await?;
    Ok(Json(state.store.list_tokens(user.id).await?))
}

/// `DELETE /api/users/{username}/tokens/{token_id}` — revoke a token.
pub async fn revoke_token(
    State(state): State<AppState>,
    Path((username, token_id)): Path<(String, Uuid)>,
) -> Result<StatusCode, AppError> {
    let user = state.store.get_user(&username).await?;
    state.store.revoke_token(user.id, token_id).await?;
    crate::audit::record(
        &state,
        None,
        Some(user.id),
        &user.username,
        "token.revoked",
        Some(&token_id.to_string()),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Request body for adding an org member.
#[derive(Debug, Deserialize)]
pub struct NewMember {
    pub username: String,
    #[serde(default)]
    pub role: Option<String>,
}

/// `POST /api/orgs/{org}/members` — add or update a user's org membership.
pub async fn add_member(
    State(state): State<AppState>,
    Path(org_name): Path<String>,
    Json(input): Json<NewMember>,
) -> Result<Json<Membership>, AppError> {
    let org = state.store.get_org(&org_name).await?;
    let user = state.store.get_user(&input.username).await?;
    let role = Role::parse(input.role.as_deref().unwrap_or("member"));
    let membership = state.store.upsert_membership(user.id, org.id, role).await?;
    crate::audit::record(
        &state,
        None,
        Some(user.id),
        "admin",
        "member.role_changed",
        Some(&format!("user:{}", user.username)),
        Some(&format!("org={} role={}", org.name, role.as_str())),
    )
    .await;
    Ok(Json(membership))
}

/// Request body for forking a repository.
#[derive(Debug, Deserialize)]
pub struct ForkInput {
    /// Target organization to fork into.
    pub org: String,
    /// Optional new name (defaults to the source name).
    #[serde(default)]
    pub name: Option<String>,
}

/// `POST /api/orgs/{org}/repos/{repo}/forks` — fork a repo into another org by
/// copying its objects + refs, then recording the new repository.
pub async fn create_fork(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(input): Json<ForkInput>,
) -> Result<Json<Repository>, AppError> {
    let _src = state.store.get_repo(&org, &repo).await?;
    let dst_org = state.store.get_org(&input.org).await?;
    let dst_name = input.name.unwrap_or_else(|| repo.clone());
    validate_name("repository", &dst_name)?;
    let default_branch = state.host.default_branch(&org, &repo).unwrap_or_else(|_| "main".into());

    let path = state
        .host
        .fork_repo(&org, &repo, &dst_org.name, &dst_name)
        .map_err(AppError::from)?;
    let created = state
        .store
        .create_repo(
            &dst_org,
            &NewRepository {
                name: dst_name,
                description: Some(format!("Fork of {org}/{repo}")),
                default_branch: Some(default_branch),
                visibility: None,
            },
            &path.to_string_lossy(),
        )
        .await?;
    crate::audit::record(
        &state,
        Some(created.id),
        None,
        "admin",
        "repo.forked",
        Some(&format!("{}/{}", dst_org.name, created.name)),
        Some(&format!("from {org}/{repo}")),
    )
    .await;
    Ok(Json(created))
}

/// Request body for registering an SSH key.
#[derive(Debug, Deserialize)]
pub struct NewSshKey {
    /// A full `authorized_keys` line, e.g. `ssh-ed25519 AAAA... comment`.
    pub key: String,
    #[serde(default)]
    pub title: Option<String>,
}

/// `POST /api/users/{username}/keys` — register an SSH public key.
pub async fn add_ssh_key(
    State(state): State<AppState>,
    Path(username): Path<String>,
    Json(input): Json<NewSshKey>,
) -> Result<Json<serde_json::Value>, AppError> {
    let user = state.store.get_user(&username).await?;
    // The stored blob is the base64 middle field of the authorized_keys line.
    let blob = input
        .key
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| AppError::bad_request("malformed public key line"))?;
    let title = input.title.as_deref().unwrap_or("key");
    state.store.add_ssh_key(user.id, title, blob).await?;
    crate::audit::record(
        &state,
        None,
        Some(user.id),
        &user.username,
        "ssh_key.added",
        Some(title),
        None,
    )
    .await;
    Ok(Json(serde_json::json!({ "username": user.username, "title": title })))
}

// --- deploy keys ------------------------------------------------------------

/// Request body for creating a deploy key.
#[derive(Debug, Deserialize)]
pub struct NewDeployKey {
    /// A full `authorized_keys` line, e.g. `ssh-ed25519 AAAA... comment`,
    /// or just the base64 blob. The blob (second space-delimited field, or the
    /// whole value if there are no spaces) is stored.
    pub key: String,
    #[serde(default = "default_deploy_key_title")]
    pub title: String,
    /// If `true` (the default) the key may only clone/fetch.
    #[serde(default = "default_read_only")]
    pub read_only: bool,
}

fn default_deploy_key_title() -> String {
    "deploy key".to_string()
}

fn default_read_only() -> bool {
    true
}

/// Response for a newly created deploy key. The `token` (for HTTP Basic auth)
/// is shown exactly once; only its hash is stored.
#[derive(Debug, Serialize)]
pub struct DeployKeyResponse {
    #[serde(flatten)]
    pub key: DeployKey,
    /// Bearer token for HTTP Basic auth (username = anything, password = token).
    /// Shown once at creation and never returned again.
    pub token: String,
}

/// `POST /api/orgs/{org}/repos/{repo}/keys` — create a deploy key.
pub async fn create_deploy_key(
    State(state): State<AppState>,
    Path((org_name, repo_name)): Path<(String, String)>,
    Json(input): Json<NewDeployKey>,
) -> Result<Json<DeployKeyResponse>, AppError> {
    let repo = state.store.get_repo(&org_name, &repo_name).await?;
    // Accept both a full authorized_keys line and a bare base64 blob.
    let blob = if input.key.contains(' ') {
        input
            .key
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| AppError::bad_request("malformed public key line"))?
            .to_string()
    } else {
        input.key.clone()
    };
    let bearer = generate_deploy_token();
    let dk = state
        .store
        .create_deploy_key(
            repo.id,
            &input.title,
            &blob,
            &hash_token(&bearer),
            input.read_only,
            None, // created_by: caller auth not yet wired at the API tier
        )
        .await?;
    crate::audit::record(
        &state,
        Some(repo.id),
        None,
        "admin",
        "deploy_key.created",
        Some(&input.title),
        Some(&format!("repo={org_name}/{repo_name} read_only={}", input.read_only)),
    )
    .await;
    Ok(Json(DeployKeyResponse { key: dk, token: bearer }))
}

/// `GET /api/orgs/{org}/repos/{repo}/keys` — list deploy keys for a repository.
pub async fn list_deploy_keys(
    State(state): State<AppState>,
    Path((org_name, repo_name)): Path<(String, String)>,
) -> Result<Json<Vec<DeployKey>>, AppError> {
    let repo = state.store.get_repo(&org_name, &repo_name).await?;
    Ok(Json(state.store.list_deploy_keys(repo.id).await?))
}

/// `DELETE /api/orgs/{org}/repos/{repo}/keys/{key_id}` — delete a deploy key.
pub async fn delete_deploy_key(
    State(state): State<AppState>,
    Path((org_name, repo_name, key_id)): Path<(String, String, Uuid)>,
) -> Result<StatusCode, AppError> {
    let repo = state.store.get_repo(&org_name, &repo_name).await?;
    state.store.delete_deploy_key(repo.id, key_id).await?;
    crate::audit::record(
        &state,
        Some(repo.id),
        None,
        "admin",
        "deploy_key.deleted",
        Some(&key_id.to_string()),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
