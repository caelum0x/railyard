//! JSON API for governance features: CI commit statuses, the audit log, search,
//! per-repo collaborators, and teams.

use axum::extract::{Path, State};
use axum::Json;
use rithub_core::{
    AccessStore, AuditStore, CheckState, NewStatus, NotifyStore, RepoRole, StatusStore, Store,
};
use serde::Deserialize;
use serde_json::json;

use crate::error::AppError;
use crate::state::AppState;

/// `GET /api/users/{username}/notifications`
pub async fn list_notifications(
    State(state): State<AppState>,
    Path(username): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let user = state.store.get_user(&username).await?;
    let items = state.store.list_for_user(user.id, 100).await?;
    let unread = state.store.unread_count(user.id).await?;
    Ok(Json(json!({ "unread": unread, "notifications": items })))
}

// --- CI statuses ------------------------------------------------------------

/// `POST /api/orgs/{org}/repos/{repo}/commits/{sha}/statuses`
pub async fn set_status(
    State(state): State<AppState>,
    Path((org, repo, sha)): Path<(String, String, String)>,
    Json(input): Json<NewStatus>,
) -> Result<Json<serde_json::Value>, AppError> {
    let meta = state.store.get_repo(&org, &repo).await?;
    let status = state.store.set_status(meta.id, &sha, &input, None).await?;
    crate::audit::record(
        &state,
        Some(meta.id),
        None,
        "ci",
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
) -> Result<Json<serde_json::Value>, AppError> {
    let meta = state.store.get_repo(&org, &repo).await?;
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
) -> Result<Json<serde_json::Value>, AppError> {
    let meta = state.store.get_repo(&org, &repo).await?;
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
    Json(input): Json<CollaboratorInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let meta = state.store.get_repo(&org, &repo).await?;
    let user = state.store.get_user(&input.username).await?;
    let role = RepoRole::parse(&input.role);
    state.store.set_collaborator(meta.id, user.id, role).await?;
    crate::audit::record(
        &state,
        Some(meta.id),
        None,
        "admin",
        "collaborator.added",
        Some(&format!("user:{}", user.username)),
        Some(&format!("role={}", role.as_str())),
    )
    .await;
    Ok(Json(json!({ "username": user.username, "role": role.as_str() })))
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
    Json(input): Json<TeamInput>,
) -> Result<Json<rithub_core::Team>, AppError> {
    let org = state.store.get_org(&org).await?;
    Ok(Json(state.store.create_team(org.id, &input.name).await?))
}

#[derive(Debug, Deserialize)]
pub struct TeamMemberInput {
    pub username: String,
}

/// `POST /api/orgs/{org}/teams/{team}/members`
pub async fn add_team_member(
    State(state): State<AppState>,
    Path((org, team)): Path<(String, String)>,
    Json(input): Json<TeamMemberInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let org = state.store.get_org(&org).await?;
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
    Json(input): Json<TeamRepoInput>,
) -> Result<Json<serde_json::Value>, AppError> {
    let org_obj = state.store.get_org(&org).await?;
    let team = state.store.get_team(org_obj.id, &team).await?;
    let repo = state.store.get_repo(&org, &input.repo).await?;
    let role = RepoRole::parse(&input.role);
    state.store.grant_team_repo(team.id, repo.id, role).await?;
    Ok(Json(json!({ "team": team.name, "repo": repo.name, "role": role.as_str() })))
}
