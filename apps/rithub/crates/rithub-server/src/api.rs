//! The forge JSON API: organizations and repositories. Real handlers backed by
//! Postgres (via `rithub-core`) and the on-disk git host (via `rithub-git`).
//!
//! Creating a repository is the load-bearing action: it validates the name,
//! initializes a **bare** git repo on disk with `rit`, and records the metadata
//! row — so a freshly-created repo is immediately hostable over git HTTP.

use axum::extract::{Path, State};
use axum::Json;
use rithub_core::{
    generate_token, hash_token, validate_name, Membership, NewOrganization, NewRepository,
    Organization, Repository, Role, SshKeyStore, Store, User,
};
use rithub_git::SeedFile;
use serde::{Deserialize, Serialize};

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
    let branch = input.branch.unwrap_or(repo.default_branch);
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
        .create_token(user.id, &input.name, &hash_token(&token))
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
    }))
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
