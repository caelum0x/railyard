//! Admin API for the job outbox.
//!
//! Routes:
//!   GET  /api/admin/jobs           — list recent jobs (filterable by status)
//!   POST /api/admin/jobs/{id}/redeliver — reset a dead/failed job to pending

use axum::extract::{Path, Query, State};
use axum::Json;
use rithub_core::OutboxStore;
use serde::Deserialize;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct JobsQuery {
    /// Filter by status: pending|running|succeeded|failed|dead
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_limit() -> i64 {
    50
}

/// `GET /api/admin/jobs`
pub async fn list_jobs(
    State(state): State<AppState>,
    Query(q): Query<JobsQuery>,
) -> Result<Json<serde_json::Value>, AppError> {
    let jobs = state
        .store
        .list_jobs(q.status.as_deref(), q.limit.min(500))
        .await?;
    Ok(Json(serde_json::json!({ "jobs": jobs
        .iter()
        .map(|j| serde_json::json!({
            "id": j.id,
            "kind": j.kind,
            "status": j.status,
            "attempts": j.attempts,
            "run_after": j.run_after,
            "created_at": j.created_at,
            "last_error": j.last_error,
        }))
        .collect::<Vec<_>>()
    })))
}

/// `POST /api/admin/jobs/{id}/redeliver`
pub async fn redeliver_job(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, AppError> {
    let job = state.store.redeliver_job(id).await?;
    Ok(Json(serde_json::json!({
        "id": job.id,
        "kind": job.kind,
        "status": job.status,
        "attempts": job.attempts,
        "run_after": job.run_after,
    })))
}
