//! Milestone API handlers: full CRUD over per-repo milestones.
//!
//! Routes (all prefixed with `/api/orgs/{org}/repos/{repo}/milestones`):
//!   POST   /                     → create_milestone
//!   GET    /                     → list_milestones
//!   GET    /{number}             → get_milestone
//!   PATCH  /{number}             → update_milestone
//!   DELETE /{number}             → delete_milestone

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use rithub_core::{MilestoneStore, MilestoneWithProgress, NewMilestone, RepoRole, UpdateMilestone};
use serde::Deserialize;

use crate::error::AppError;
use crate::state::AppState;

// --- helpers -----------------------------------------------------------------

/// Repo id the caller may read (listing/viewing milestones).
async fn readable_repo_id(
    state: &AppState,
    headers: &HeaderMap,
    org: &str,
    repo: &str,
) -> Result<uuid::Uuid, AppError> {
    Ok(crate::authz::repo_for_read(state, headers, org, repo).await?.0.id)
}

/// Repo id the caller may manage milestones on (Write).
async fn writable_repo_id(
    state: &AppState,
    headers: &HeaderMap,
    org: &str,
    repo: &str,
) -> Result<uuid::Uuid, AppError> {
    Ok(crate::authz::repo_for_role(state, headers, org, repo, RepoRole::Write)
        .await?
        .0
        .id)
}

// --- request bodies ----------------------------------------------------------

/// `POST /milestones` — create a milestone.
#[derive(Debug, Deserialize)]
pub struct CreateMilestoneInput {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    #[serde(with = "time::serde::rfc3339::option")]
    pub due_on: Option<time::OffsetDateTime>,
}

// --- handlers ----------------------------------------------------------------

pub async fn create_milestone(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<CreateMilestoneInput>,
) -> Result<Json<MilestoneWithProgress>, AppError> {
    let rid = writable_repo_id(&state, &headers, &org, &repo).await?;
    if input.title.trim().is_empty() {
        return Err(AppError::bad_request("milestone title is required"));
    }
    let milestone = state
        .store
        .create_milestone(
            rid,
            &NewMilestone {
                title: input.title,
                description: input.description,
                due_on: input.due_on,
            },
        )
        .await?;
    // Re-fetch with progress counts (0/0 on a freshly created milestone).
    let with_progress = state.store.get_milestone(rid, milestone.number).await?;
    Ok(Json(with_progress))
}

pub async fn list_milestones(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Json<Vec<MilestoneWithProgress>>, AppError> {
    let rid = readable_repo_id(&state, &headers, &org, &repo).await?;
    Ok(Json(state.store.list_milestones(rid).await?))
}

pub async fn get_milestone(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
) -> Result<Json<MilestoneWithProgress>, AppError> {
    let rid = readable_repo_id(&state, &headers, &org, &repo).await?;
    Ok(Json(state.store.get_milestone(rid, number).await?))
}

pub async fn update_milestone(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
    Json(input): Json<UpdateMilestone>,
) -> Result<Json<MilestoneWithProgress>, AppError> {
    let rid = writable_repo_id(&state, &headers, &org, &repo).await?;
    state.store.update_milestone(rid, number, &input).await?;
    // Return the updated milestone with fresh progress counts.
    Ok(Json(state.store.get_milestone(rid, number).await?))
}

pub async fn delete_milestone(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    let rid = writable_repo_id(&state, &headers, &org, &repo).await?;
    state.store.delete_milestone(rid, number).await?;
    Ok(StatusCode::NO_CONTENT)
}
