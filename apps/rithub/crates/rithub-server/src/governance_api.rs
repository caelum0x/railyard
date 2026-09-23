//! JSON API for governance features: CI commit statuses, the audit log, search,
//! per-repo collaborators, and teams.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use rithub_core::{
    AccessStore, AuditStore, CheckState, DirectoryStore, NewStatus, NotifyStore, RepoRole, Role,
    StatusStore, Store,
};
use uuid::Uuid;

use crate::auth::Caller;
use crate::authz::{self, repo_for_read, repo_for_role};
use serde::Deserialize;
use serde_json::json;

use crate::error::AppError;
use crate::state::AppState;

/// `GET /api/users/{username}/notifications`
pub async fn list_notifications(
    State(state): State<AppState>,
    Path(username): Path<String>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let (_, user) = authz::self_or_admin(&state, &headers, &username).await?;
    let items = state.store.list_for_user(user.id, 100).await?;
    let unread = state.store.unread_count(user.id).await?;
    Ok(Json(json!({ "unread": unread, "notifications": items })))
}

/// `POST /api/users/{username}/notifications/read` — mark all read.
pub async fn mark_all_notifications_read(
    State(state): State<AppState>,
    Path(username): Path<String>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let (_, user) = authz::self_or_admin(&state, &headers, &username).await?;
    state.store.mark_all_read(user.id).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /api/users/{username}/notifications/{id}/read` — mark one read.
pub async fn mark_notification_read(
    State(state): State<AppState>,
    Path((username, id)): Path<(String, Uuid)>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let (_, user) = authz::self_or_admin(&state, &headers, &username).await?;
    state.store.mark_notification_read(user.id, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

// --- CI statuses ------------------------------------------------------------

/// `POST /api/orgs/{org}/repos/{repo}/commits/{sha}/statuses`
pub async fn set_status(
    State(state): State<AppState>,
    Path((org, repo, sha)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(input): Json<NewStatus>,
) -> Result<Json<serde_json::Value>, AppError> {
    if sha.len() < 7 || sha.len() > 64 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(AppError::bad_request("sha must be a hex commit id"));
    }
    // Writers, or a write-enabled deploy key for this repo (CI bots).
    let (meta, creator, actor_name) = match authz::api_caller(&state, &headers).await? {
        Some(Caller::DeployKey { repo_id, read_only }) => {
            let meta = state.store.get_repo(&org, &repo).await?;
            if repo_id != meta.id || read_only {
                return Err(AppError::forbidden("deploy key cannot set statuses on this repository"));
            }
            (meta, None, "deploy-key".to_string())
        }
        _ => {
            let (meta, user) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Write).await?;
            let name = user.username.clone();
            (meta, Some(user.id), name)
        }
    };
    let status = state.store.set_status(meta.id, &sha, &input, creator).await?;
    crate::audit::record(
        &state,
        Some(meta.id),
        creator,
        &actor_name,
        "status.set",
        Some(&sha[..sha.len().min(8)]),
        Some(&format!("{}={}", status.context, status.state.as_str())),
    )
    .await;
    Ok(Json(serde_json::to_value(status).unwrap()))
}

/// `GET /api/orgs/{org}/repos/{repo}/commits/{sha}/status` — combined status.
pub async fn get_status(
    State(state): State<AppState>,
    Path((org, repo, sha)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let (meta, _) = repo_for_read(&state, &headers, &org, &repo).await?;
    let statuses = state.store.latest_statuses(meta.id, &sha).await?;
    // Combined state: failure/error wins, else success only if there are checks
    // and all succeed, else pending.
    let combined = if statuses.is_empty() {
        "pending"
    } else if statuses
        .iter()
        .any(|s| matches!(s.state, CheckState::Failure | CheckState::Error))
    {
        "failure"
    } else if statuses.iter().all(|s| s.state == CheckState::Success) {
        "success"
    } else {
        "pending"
    };
    Ok(Json(json!({ "state": combined, "statuses": statuses })))
}

// --- audit ------------------------------------------------------------------

/// `GET /api/orgs/{org}/repos/{repo}/audit`
pub async fn list_audit(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let (meta, _) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Admin).await?;
    let events = state.store.list_repo_events(meta.id, 200).await?;
    Ok(Json(json!({ "events": events })))
}

// Note: full-text search moved to `crate::search_api` (Tantivy/Postgres backends
// with permission scoping). The former ILIKE handler lived here.

// --- collaborators ----------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CollaboratorInput {
    pub username: String,
    pub role: String,
}

/// `POST /api/orgs/{org}/repos/{repo}/collaborators`
pub async fn add_collaborator(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<CollaboratorInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (meta, actor) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Admin).await?;
    let role = parse_repo_role(&input.role)?;
    let username = grant_collaborator(&state, &meta, &actor, &input.username, role).await?;
    Ok(Json(json!({ "username": username, "role": role.as_str() })))
}

/// Strictly parse a repo role (`read|write|admin`).
pub fn parse_repo_role(value: &str) -> Result<RepoRole, AppError> {
    match value.trim() {
        "read" => Ok(RepoRole::Read),
        "write" => Ok(RepoRole::Write),
        "admin" => Ok(RepoRole::Admin),
        _ => Err(AppError::bad_request("role must be read, write, or admin")),
    }
}

/// Grant a collaborator role (shared with the web settings page).
pub async fn grant_collaborator(
    state: &AppState,
    meta: &rithub_core::Repository,
    actor: &rithub_core::User,
    username: &str,
    role: RepoRole,
) -> Result<String, AppError> {
    let user = state.store.get_user(username.trim()).await?;
    state.store.set_collaborator(meta.id, user.id, role).await?;
    crate::audit::record(
        state,
        Some(meta.id),
        Some(actor.id),
        &actor.username,
        "collaborator.added",
        Some(&format!("user:{}", user.username)),
        Some(&format!("role={}", role.as_str())),
    )
    .await;
    Ok(user.username)
}

/// Revoke a collaborator (shared with the web settings page).
pub async fn revoke_collaborator(
    state: &AppState,
    meta: &rithub_core::Repository,
    actor: &rithub_core::User,
    username: &str,
) -> Result<(), AppError> {
    let user = state.store.get_user(username).await?;
    state.store.remove_collaborator(meta.id, user.id).await?;
    crate::audit::record(
        state,
        Some(meta.id),
        Some(actor.id),
        &actor.username,
        "collaborator.removed",
        Some(&format!("user:{}", user.username)),
        None,
    )
    .await;
    Ok(())
}

/// `GET /api/orgs/{org}/repos/{repo}/collaborators` (Admin).
pub async fn list_collaborators(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
    let (meta, _) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Admin).await?;
    let collaborators = state.store.list_collaborator_users(meta.id).await?;
    Ok(Json(json!({
        "collaborators": collaborators.iter().map(|(u, r)| json!({"username": u.username, "role": r.as_str()})).collect::<Vec<_>>()
    })))
}

/// `DELETE /api/orgs/{org}/repos/{repo}/collaborators/{username}` (Admin).
pub async fn remove_collaborator(
    State(state): State<AppState>,
    Path((org, repo, username)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let (meta, actor) = repo_for_role(&state, &headers, &org, &repo, RepoRole::Admin).await?;
    revoke_collaborator(&state, &meta, &actor, &username).await?;
    Ok(StatusCode::NO_CONTENT)
}

// --- teams ------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct TeamInput {
    pub name: String,
}

/// `POST /api/orgs/{org}/teams`
pub async fn create_team(
    State(state): State<AppState>,
    Path(org): Path<String>,
    headers: HeaderMap,
    Json(input): Json<TeamInput>,
) -> Result<Json<rithub_core::Team>, AppError> {
    let (org, _) = authz::org_for_role(&state, &headers, &org, Role::Admin).await?;
    rithub_core::validate_name("team", input.name.trim())?;
    Ok(Json(state.store.create_team(org.id, input.name.trim()).await?))
}

/// `GET /api/orgs/{org}/teams` (members).
pub async fn list_teams(
    State(state): State<AppState>,
    Path(org): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<rithub_core::Team>>, AppError> {
    let (org, _) = authz::org_for_role(&state, &headers, &org, Role::Member).await?;
    Ok(Json(state.store.list_teams(org.id).await?))
}

#[derive(Debug, Deserialize)]
pub struct TeamMemberInput {
    pub username: String,
}

/// `POST /api/orgs/{org}/teams/{team}/members`
pub async fn add_team_member(
    State(state): State<AppState>,
    Path((org, team)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<TeamMemberInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (org, _) = authz::org_for_role(&state, &headers, &org, Role::Admin).await?;
    let team = state.store.get_team(org.id, &team).await?;
    let user = state.store.get_user(&input.username).await?;
    state.store.add_team_member(team.id, user.id).await?;
    Ok(Json(json!({ "team": team.name, "member": user.username })))
}

#[derive(Debug, Deserialize)]
pub struct TeamRepoInput {
    pub repo: String,
    pub role: String,
}

/// `POST /api/orgs/{org}/teams/{team}/repos`
pub async fn grant_team_repo(
    State(state): State<AppState>,
    Path((org, team)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<TeamRepoInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let (org_obj, _) = authz::org_for_role(&state, &headers, &org, Role::Admin).await?;
    let team = state.store.get_team(org_obj.id, &team).await?;
    let repo = state.store.get_repo(&org, &input.repo).await?;
    let role = parse_repo_role(&input.role)?;
    state.store.grant_team_repo(team.id, repo.id, role).await?;
    Ok(Json(json!({ "team": team.name, "repo": repo.name, "role": role.as_str() })))
}
