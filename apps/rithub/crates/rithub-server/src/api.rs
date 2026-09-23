//! The forge JSON API: accounts, tokens, SSH keys, organizations, membership,
//! repositories, forks, and deploy keys. Real handlers backed by Postgres (via
//! `rithub-core`) and the on-disk git host (via `rithub-git`).
//!
//! Every mutating route is authenticated and authorized through
//! [`crate::authz`]; reads of private repositories are hidden (404) from
//! callers without access. Account operations are self-service (a user manages
//! their own tokens/keys) or site-admin.
//!
//! Creating a repository is the load-bearing action: it enforces the org's plan
//! quota ([`crate::billing`]), initializes a **bare** git repo on disk with
//! `rit`, and records the metadata row — so a freshly-created repo is
//! immediately hostable over git HTTP/SSH.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use rithub_core::{
    generate_deploy_token, hash_token, validate_name, DeployKey,
    DeployKeyStore, DirectoryStore, Membership, NewOrganization, NewRepository, Organization,
    RepoRole, Repository, ReviewStore, Role, SshKeyStore, Store, UpdateRepository, User,
    Visibility,
};
use rithub_git::SeedFile;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::Caller;
use crate::authz::{self, repo_for_read, repo_for_role};
use crate::error::AppError;
use crate::state::AppState;

/// Names that collide with top-level web routes and so cannot be org/user names.
const RESERVED_NAMES: &[&str] = &[
    "api", "static", "login", "logout", "signup", "search", "notifications", "settings", "new",
    "orgs", "graphql", "healthz", "explore", "admin", "-",
];

/// Validate an org/user namespace name (charset + reserved words).
pub fn validate_namespace(kind: &'static str, name: &str) -> Result<(), AppError> {
    validate_name(kind, name)?;
    if RESERVED_NAMES.contains(&name.to_ascii_lowercase().as_str()) {
        return Err(AppError::bad_request(format!("'{name}' is a reserved name")));
    }
    Ok(())
}

// Account handlers (users, tokens, SSH keys) live in `accounts_api` and are
// re-exported so routes and callers keep using `api::...`.
pub use crate::accounts_api::*;

// --- organizations ----------------------------------------------------------

/// Create an org owned by `owner` (shared with the web "new organization" form).
pub async fn create_org_for(
    state: &AppState,
    owner: &User,
    input: &NewOrganization,
) -> Result<Organization, AppError> {
    validate_namespace("organization", &input.name)?;
    let org = state.store.create_org(input).await?;
    state.store.upsert_membership(owner.id, org.id, Role::Owner).await?;
    crate::audit::record(state, None, Some(owner.id), &owner.username, "org.created", Some(&org.name), None)
        .await;
    billing_seat(&org, owner, "added");
    Ok(org)
}

fn billing_seat(org: &Organization, user: &User, action: &str) {
    crate::billing::meter(
        &org.name,
        crate::billing::METRIC_SEAT,
        format!("seat|{}|{}|{}|{}", org.id, user.id, action, OffsetDateTime::now_utc().unix_timestamp()),
        serde_json::json!({ "user": user.username, "action": action }),
    );
}

/// `POST /api/orgs` — create an organization; the caller becomes its owner.
pub async fn create_org(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<NewOrganization>,
) -> Result<Json<Organization>, AppError> {
    let caller = authz::require_user(&state, &headers).await?;
    let user = authz::caller_user(&caller).clone();
    Ok(Json(create_org_for(&state, &user, &input).await?))
}

/// `GET /api/orgs` — list organizations.
pub async fn list_orgs(State(state): State<AppState>) -> Result<Json<Vec<Organization>>, AppError> {
    Ok(Json(state.store.list_orgs().await?))
}

/// `GET /api/orgs/{org}` — fetch one organization.
pub async fn get_org(
    State(state): State<AppState>,
    Path(org): Path<String>,
) -> Result<Json<Organization>, AppError> {
    Ok(Json(state.store.get_org(&org).await?))
}

/// Request body for adding an org member.
#[derive(Debug, Deserialize)]
pub struct NewMember {
    pub username: String,
    #[serde(default)]
    pub role: Option<String>,
}

/// Add or change a member's role (org owners only; shared with the web UI).
pub async fn set_member_role(
    state: &AppState,
    org: &Organization,
    actor: &User,
    username: &str,
    role: Role,
) -> Result<Membership, AppError> {
    let user = state.store.get_user(username.trim()).await?;
    let previous = state.store.get_membership(user.id, org.id).await?;
    if previous == Some(Role::Owner)
        && role != Role::Owner
        && state.store.count_org_owners(org.id).await? <= 1
    {
        return Err(AppError::conflict("an organization must keep at least one owner"));
    }
    let membership = state.store.upsert_membership(user.id, org.id, role).await?;
    crate::audit::record(
        state,
        None,
        Some(actor.id),
        &actor.username,
        "member.role_changed",
        Some(&format!("user:{}", user.username)),
        Some(&format!("org={} role={}", org.name, role.as_str())),
    )
    .await;
    if previous.is_none() {
        billing_seat(org, &user, "added");
    }
    Ok(membership)
}

/// Remove a member (org owners only; shared with the web UI).
pub async fn remove_member(
    state: &AppState,
    org: &Organization,
    actor: &User,
    username: &str,
) -> Result<(), AppError> {
    let user = state.store.get_user(username).await?;
    if state.store.get_membership(user.id, org.id).await? == Some(Role::Owner)
        && state.store.count_org_owners(org.id).await? <= 1
    {
        return Err(AppError::conflict("an organization must keep at least one owner"));
    }
    state.store.remove_membership(user.id, org.id).await?;
    crate::audit::record(
        state,
        None,
        Some(actor.id),
        &actor.username,
        "member.removed",
        Some(&format!("user:{}", user.username)),
        Some(&format!("org={}", org.name)),
    )
    .await;
    billing_seat(org, &user, "removed");
    Ok(())
}

/// `POST /api/orgs/{org}/members` — add or update a membership (owners only).
pub async fn add_member(
    State(state): State<AppState>,
    Path(org_name): Path<String>,
    headers: HeaderMap,
    Json(input): Json<NewMember>,
) -> Result<Json<Membership>, AppError> {
    let (org, actor) = authz::org_for_role(&state, &headers, &org_name, Role::Owner).await?;
    let role = Role::parse(input.role.as_deref().unwrap_or("member"));
    Ok(Json(set_member_role(&state, &org, &actor, &input.username, role).await?))
}

/// `GET /api/orgs/{org}/members` — list members (members only).
pub async fn list_members(
    State(state): State<AppState>,
    Path(org_name): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let (org, _) = authz::org_for_role(&state, &headers, &org_name, Role::Member).await?;
    let members = state.store.list_org_members(org.id).await?;
    Ok(Json(serde_json::json!({
        "members": members.iter().map(|(u, r)| serde_json::json!({"username": u.username, "role": r.as_str()})).collect::<Vec<_>>()
    })))
}

/// `DELETE /api/orgs/{org}/members/{username}` — remove a member (owners only).
pub async fn delete_member(
    State(state): State<AppState>,
    Path((org_name, username)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let (org, actor) = authz::org_for_role(&state, &headers, &org_name, Role::Owner).await?;
    remove_member(&state, &org, &actor, &username).await?;
    Ok(StatusCode::NO_CONTENT)
}

// --- repositories -----------------------------------------------------------

fn meter_private_repo(org: &str, repo: &Repository, op: &str) {
    crate::billing::meter(
        org,
        crate::billing::METRIC_PRIVATE_REPO,
        format!("private_repo|{}|{op}|{}", repo.id, OffsetDateTime::now_utc().unix_timestamp()),
        serde_json::json!({ "repo": format!("{org}/{}", repo.name), "op": op }),
    );
}

/// Create a repository in `org` (quota-checked; shared with the web UI).
pub async fn create_repo_in(
    state: &AppState,
    org: &Organization,
    actor: &User,
    input: &NewRepository,
) -> Result<Repository, AppError> {
    validate_name("repository", &input.name)?;
    if let Some(branch) = &input.default_branch {
        validate_name("branch", branch)?;
    }
    if input.visibility == Some(Visibility::Private) {
        crate::billing::ensure_private_repo_quota(state, org).await?;
    }
    let default_branch = input.default_branch.as_deref().unwrap_or("main");
    let path = state.host.init_bare(&org.name, &input.name, default_branch)?;
    let disk_path = path.to_string_lossy().into_owned();
    let repo = state.store.create_repo(org, input, &disk_path).await?;
    crate::search_index::index_repo(state, &org.name, &repo).await;
    crate::audit::record(
        state,
        Some(repo.id),
        Some(actor.id),
        &actor.username,
        "repo.created",
        Some(&format!("{}/{}", org.name, repo.name)),
        Some(repo.visibility.as_str()),
    )
    .await;
    if repo.visibility == Visibility::Private {
        meter_private_repo(&org.name, &repo, "+1");
    }
    Ok(repo)
}

/// `POST /api/orgs/{org}/repos` — create a repository (org admins/owners).
pub async fn create_repo(
    State(state): State<AppState>,
    Path(org_name): Path<String>,
    headers: HeaderMap,
    Json(input): Json<NewRepository>,
) -> Result<Json<Repository>, AppError> {
    let (org, actor) = authz::org_for_role(&state, &headers, &org_name, Role::Admin).await?;
    Ok(Json(create_repo_in(&state, &org, &actor, &input).await?))
}

/// Repositories of `org` the (optional) user may see.
pub async fn visible_repos(
    state: &AppState,
    org: &Organization,
    user: Option<&User>,
) -> Result<Vec<Repository>, AppError> {
    let repos = state.store.list_repos(org).await?;
    let mut out = Vec::with_capacity(repos.len());
    for repo in repos {
        if authz::user_can_read(state, user, &repo).await {
            out.push(repo);
        }
    }
    Ok(out)
}

/// `GET /api/orgs/{org}/repos` — list the repositories the caller can see.
pub async fn list_repos(
    State(state): State<AppState>,
    Path(org_name): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<Repository>>, AppError> {
    let caller = authz::api_caller(&state, &headers).await?;
    let org = state.store.get_org(&org_name).await?;
    Ok(Json(visible_repos(&state, &org, caller.as_ref().and_then(Caller::user)).await?))
}

/// `GET /api/orgs/{org}/repos/{repo}` — repository metadata.
pub async fn get_repo(
    State(state): State<AppState>,
    Path((org_name, repo_name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Repository>, AppError> {
    let (meta, _) = repo_for_read(&state, &headers, &org_name, &repo_name).await?;
    Ok(Json(meta))
}

/// Apply repository settings (rename, description, default branch, visibility),
/// moving the bare repo on disk on rename and enforcing the private-repo quota.
pub async fn update_repo_settings(
    state: &AppState,
    org_name: &str,
    meta: &Repository,
    actor: &User,
    input: &UpdateRepository,
) -> Result<Repository, AppError> {
    if let Some(new_name) = &input.name {
        validate_name("repository", new_name)?;
    }
    if let Some(branch) = &input.default_branch {
        state
            .host
            .resolve_branch(org_name, &meta.name, branch)
            .map_err(|_| AppError::bad_request(format!("branch not found: {branch}")))?;
    }
    let org = state.store.get_org(org_name).await?;
    let going_private = input.visibility == Some(Visibility::Private)
        && meta.visibility != Visibility::Private;
    let going_public =
        input.visibility == Some(Visibility::Public) && meta.visibility == Visibility::Private;
    if going_private {
        crate::billing::ensure_private_repo_quota(state, &org).await?;
    }

    // Rename: move the bare repo first so the row never points at a missing dir.
    let renamed = input.name.as_deref().filter(|n| *n != meta.name);
    let moved = match renamed {
        Some(new_name) => {
            let from = state.host.repo_path(org_name, &meta.name)?;
            let to = state.host.repo_path(org_name, new_name)?;
            if to.exists() {
                return Err(AppError::conflict(format!("repository already exists: {new_name}")));
            }
            std::fs::rename(&from, &to)
                .map_err(|e| AppError::internal(format!("moving repository on disk: {e}")))?;
            Some((from, to))
        }
        None => None,
    };
    let updated = match state.store.update_repo(meta.id, input).await {
        Ok(repo) => repo,
        Err(e) => {
            if let Some((from, to)) = &moved {
                let _ = std::fs::rename(to, from);
            }
            return Err(e.into());
        }
    };
    if let Some((_, to)) = &moved {
        state
            .store
            .set_repo_disk_path(updated.id, &to.to_string_lossy())
            .await?;
    }
    if let Some(branch) = &input.default_branch {
        if let Err(e) = state.host.set_default_branch(org_name, &updated.name, branch) {
            tracing::warn!(error = %e, "updating HEAD for default branch failed");
        }
    }
    crate::search_index::index_repo(state, org_name, &updated).await;
    crate::audit::record(
        state,
        Some(updated.id),
        Some(actor.id),
        &actor.username,
        "repo.updated",
        Some(&format!("{org_name}/{}", updated.name)),
        Some(&format!("visibility={}", updated.visibility.as_str())),
    )
    .await;
    if going_private {
        meter_private_repo(org_name, &updated, "+1");
    } else if going_public {
        meter_private_repo(org_name, &updated, "-1");
    }
    Ok(updated)
}

/// `PATCH /api/orgs/{org}/repos/{repo}` — update repository settings (Admin).
pub async fn update_repo(
    State(state): State<AppState>,
    Path((org_name, repo_name)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<UpdateRepository>,
) -> Result<Json<Repository>, AppError> {
    let (meta, actor) = repo_for_role(&state, &headers, &org_name, &repo_name, RepoRole::Admin).await?;
    Ok(Json(update_repo_settings(&state, &org_name, &meta, &actor, &input).await?))
}

/// Delete a repository: row (cascading every dependent record), search
/// documents, and the bare repo on disk. Shared with the web settings page.
pub async fn delete_repo_fully(
    state: &AppState,
    org_name: &str,
    meta: &Repository,
    actor: &User,
) -> Result<(), AppError> {
    state.store.delete_repo(meta.id).await?;
    crate::search_index::remove_repo(state, meta.id).await;
    let path = state.host.repo_path(org_name, &meta.name)?;
    if let Err(e) = std::fs::remove_dir_all(&path) {
        tracing::warn!(error = %e, path = %path.display(), "removing repository directory failed");
    }
    crate::audit::record(
        state,
        None,
        Some(actor.id),
        &actor.username,
        "repo.deleted",
        Some(&format!("{org_name}/{}", meta.name)),
        None,
    )
    .await;
    if meta.visibility == Visibility::Private {
        meter_private_repo(org_name, meta, "-1");
    }
    Ok(())
}

/// `DELETE /api/orgs/{org}/repos/{repo}` — delete a repository (Admin).
pub async fn delete_repo(
    State(state): State<AppState>,
    Path((org_name, repo_name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let (meta, actor) = repo_for_role(&state, &headers, &org_name, &repo_name, RepoRole::Admin).await?;
    delete_repo_fully(&state, &org_name, &meta, &actor).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Request body for seeding a commit.
#[derive(Debug, Deserialize)]
pub struct SeedRequest {
    pub files: Vec<SeedFileInput>,
    pub message: String,
    #[serde(default)]
    pub branch: Option<String>,
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
/// `rit` (Write). The commit is authored as the authenticated user.
pub async fn seed_repo(
    State(state): State<AppState>,
    Path((org_name, repo_name)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<SeedRequest>,
) -> Result<Json<SeedResponse>, AppError> {
    let (repo, actor) = repo_for_role(&state, &headers, &org_name, &repo_name, RepoRole::Write).await?;
    if input.files.is_empty() {
        return Err(AppError::bad_request("seed requires at least one file"));
    }
    if input.message.trim().is_empty() {
        return Err(AppError::bad_request("commit message is required"));
    }
    let branch = input.branch.unwrap_or_else(|| repo.default_branch.clone());
    if state
        .store
        .list_push_protected_branches(repo.id)
        .await?
        .contains(&branch)
        && !matches!(authz::user_repo_role(&state, &actor, &repo).await, Some(RepoRole::Admin))
    {
        return Err(AppError::forbidden(format!("branch '{branch}' is protected")));
    }
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
        &actor.username,
        &actor.email,
    )?;
    crate::search_index::index_repo_git(&state, &org_name, &repo).await;
    crate::audit::record(
        &state,
        Some(repo.id),
        Some(actor.id),
        &actor.username,
        "push",
        Some(&format!("refs/heads/{branch}")),
        Some("via seed API"),
    )
    .await;
    Ok(Json(SeedResponse { commit, branch }))
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

/// Fork `src` into `dst_org` (shared with the web "Fork" button). The caller
/// must be able to read the source and create repos in the destination org.
pub async fn fork_into(
    state: &AppState,
    org: &str,
    src: &Repository,
    dst_org: &Organization,
    dst_name: &str,
    actor: &User,
) -> Result<Repository, AppError> {
    validate_name("repository", dst_name)?;
    if src.visibility == Visibility::Private {
        crate::billing::ensure_private_repo_quota(state, dst_org).await?;
    }
    let default_branch = state
        .host
        .default_branch(org, &src.name)
        .unwrap_or_else(|_| "main".into());
    let path = state
        .host
        .fork_repo(org, &src.name, &dst_org.name, dst_name)
        .map_err(AppError::from)?;
    let created = state
        .store
        .create_repo(
            dst_org,
            &NewRepository {
                name: dst_name.to_string(),
                description: Some(format!("Fork of {org}/{}", src.name)),
                default_branch: Some(default_branch),
                // A fork of a private repo stays private.
                visibility: Some(src.visibility),
            },
            &path.to_string_lossy(),
        )
        .await?;
    state.store.set_forked_from(created.id, src.id).await?;
    crate::search_index::index_repo(state, &dst_org.name, &created).await;
    crate::audit::record(
        state,
        Some(created.id),
        Some(actor.id),
        &actor.username,
        "repo.forked",
        Some(&format!("{}/{}", dst_org.name, created.name)),
        Some(&format!("from {org}/{}", src.name)),
    )
    .await;
    if created.visibility == Visibility::Private {
        meter_private_repo(&dst_org.name, &created, "+1");
    }
    Ok(created)
}

/// `POST /api/orgs/{org}/repos/{repo}/forks` — fork a repo into another org.
pub async fn create_fork(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<ForkInput>,
) -> Result<Json<Repository>, AppError> {
    let (src, _) = repo_for_read(&state, &headers, &org, &repo).await?;
    let (dst_org, actor) = authz::org_for_role(&state, &headers, &input.org, Role::Admin).await?;
    let dst_name = input.name.unwrap_or_else(|| repo.clone());
    Ok(Json(fork_into(&state, &org, &src, &dst_org, &dst_name, &actor).await?))
}

// --- deploy keys ------------------------------------------------------------

/// Request body for creating a deploy key.
#[derive(Debug, Deserialize)]
pub struct NewDeployKey {
    /// A full `authorized_keys` line, e.g. `ssh-ed25519 AAAA... comment`.
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
    pub token: String,
}

/// Create a deploy key (shared with the web settings page).
pub async fn add_deploy_key(
    state: &AppState,
    org_name: &str,
    repo: &Repository,
    actor: &User,
    input: &NewDeployKey,
) -> Result<DeployKeyResponse, AppError> {
    let blob = parse_public_key(&input.key)?;
    if state.store.find_deploy_key_by_blob(&blob).await?.is_some()
        || state.store.find_user_by_ssh_key(&blob).await?.is_some()
    {
        return Err(AppError::conflict("that key is already registered"));
    }
    let title = if input.title.trim().is_empty() { "deploy key" } else { input.title.trim() };
    let bearer = generate_deploy_token();
    let dk = state
        .store
        .create_deploy_key(repo.id, title, &blob, &hash_token(&bearer), input.read_only, Some(actor.id))
        .await?;
    crate::audit::record(
        state,
        Some(repo.id),
        Some(actor.id),
        &actor.username,
        "deploy_key.created",
        Some(title),
        Some(&format!("repo={org_name}/{} read_only={}", repo.name, input.read_only)),
    )
    .await;
    Ok(DeployKeyResponse { key: dk, token: bearer })
}

/// `POST /api/orgs/{org}/repos/{repo}/keys` — create a deploy key (Admin).
pub async fn create_deploy_key(
    State(state): State<AppState>,
    Path((org_name, repo_name)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<NewDeployKey>,
) -> Result<Json<DeployKeyResponse>, AppError> {
    let (repo, actor) = repo_for_role(&state, &headers, &org_name, &repo_name, RepoRole::Admin).await?;
    Ok(Json(add_deploy_key(&state, &org_name, &repo, &actor, &input).await?))
}

/// `GET /api/orgs/{org}/repos/{repo}/keys` — list deploy keys (Admin).
pub async fn list_deploy_keys(
    State(state): State<AppState>,
    Path((org_name, repo_name)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Vec<DeployKey>>, AppError> {
    let (repo, _) = repo_for_role(&state, &headers, &org_name, &repo_name, RepoRole::Admin).await?;
    Ok(Json(state.store.list_deploy_keys(repo.id).await?))
}

/// `DELETE /api/orgs/{org}/repos/{repo}/keys/{key_id}` — delete a deploy key (Admin).
pub async fn delete_deploy_key(
    State(state): State<AppState>,
    Path((org_name, repo_name, key_id)): Path<(String, String, Uuid)>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let (repo, actor) = repo_for_role(&state, &headers, &org_name, &repo_name, RepoRole::Admin).await?;
    state.store.delete_deploy_key(repo.id, key_id).await?;
    crate::audit::record(
        &state,
        Some(repo.id),
        Some(actor.id),
        &actor.username,
        "deploy_key.deleted",
        Some(&key_id.to_string()),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
