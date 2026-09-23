//! Milestone API handlers: full CRUD over per-repo milestones.
//!
//! Routes (all prefixed with `/api/orgs/{org}/repos/{repo}/milestones`):
//!   POST   /                     → create_milestone
//!   GET    /                     → list_milestones
//!   GET    /{number}             → get_milestone
//!   PATCH  /{number}             → update_milestone
//!   DELETE /{number}             → delete_milestone

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use rithub_core::{MilestoneStore, MilestoneWithProgress, NewMilestone, Store, UpdateMilestone};
use serde::Deserialize;

use crate::error::AppError;
use crate::state::AppState;

// --- helpers -----------------------------------------------------------------

async fn repo_id(state: &AppState, org: &str, repo: &str) -> Result<uuid::Uuid, AppError> {
    Ok(state.store.get_repo(org, repo).await?.id)
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
    Json(input): Json<CreateMilestoneInput>,
) -> Result<Json<MilestoneWithProgress>, AppError> {
    let rid = repo_id(&state, &org, &repo).await?;
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
) -> Result<Json<Vec<MilestoneWithProgress>>, AppError> {
    let rid = repo_id(&state, &org, &repo).await?;
    Ok(Json(state.store.list_milestones(rid).await?))
}

pub async fn get_milestone(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<MilestoneWithProgress>, AppError> {
    let rid = repo_id(&state, &org, &repo).await?;
    Ok(Json(state.store.get_milestone(rid, number).await?))
}

pub async fn update_milestone(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
    Json(input): Json<UpdateMilestone>,
) -> Result<Json<MilestoneWithProgress>, AppError> {
    let rid = repo_id(&state, &org, &repo).await?;
    state.store.update_milestone(rid, number, &input).await?;
    // Return the updated milestone with fresh progress counts.
    Ok(Json(state.store.get_milestone(rid, number).await?))
}

pub async fn delete_milestone(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
) -> Result<StatusCode, AppError> {
    let rid = repo_id(&state, &org, &repo).await?;
    state.store.delete_milestone(rid, number).await?;
    Ok(StatusCode::NO_CONTENT)
}
