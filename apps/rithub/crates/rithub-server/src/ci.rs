//! CI runner + check-run HTTP surface: the external-runner contract.
//!
//! ## Registration
//! `POST /api/orgs/{org}/repos/{repo}/runners` mints a one-time bearer token
//! (only its hash is stored). A runner presents that token to report status.
//!
//! ## Dispatch
//! On push (`git_http`) and PR open (`collab_api`) the server enqueues a
//! [`KIND_CHECK_RUN_DISPATCH`] outbox job. The worker (see `jobs/mod.rs`) seeds a
//! `queued` check run for the head sha and fans a `check_run` webhook event out
//! to subscribers — that webhook is how an external runner learns there is work.
//!
//! ## Reporting
//! A runner authenticates with its bearer token and calls
//! `POST   .../check-runs`         to create a run, and
//! `PATCH  .../check-runs/{id}`    to advance it (in_progress → completed).
//!
//! ## Surfacing
//! `GET .../commits/{sha}/check-runs` and `GET .../pulls/{n}/check-runs` return
//! the latest run per check name plus a combined roll-up state.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::Json;
use rithub_core::{
    generate_runner_token, hash_token, CheckConclusion, CheckRun, CheckRunPatch, CheckRunStatus,
    CiStore, NewCheckRun, OutboxStore, PullRequestStore, Runner, Store, KIND_CHECK_RUN_DISPATCH,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use crate::error::AppError;
use crate::state::AppState;

/// The default check name seeded by the dispatcher before a runner claims work.
pub const DEFAULT_CHECK_NAME: &str = "ci";

// ---------------------------------------------------------------------------
// Dispatch: enqueue a check-run job (called from git_http push + collab PR).
// ---------------------------------------------------------------------------

/// Enqueue a [`KIND_CHECK_RUN_DISPATCH`] job for `head_sha`. Non-fatal: a failed
/// enqueue is logged and never propagates to the triggering request.
pub fn dispatch_check_runs(
    state: &AppState,
    repo_id: Uuid,
    org: &str,
    repo: &str,
    head_sha: &str,
    reference: &str,
    event: &str,
) {
    let state = state.clone();
    let payload = json!({
        "repo_id": repo_id,
        "org": org,
        "repo": repo,
        "head_sha": head_sha,
        "ref": reference,
        "event": event,
    });
    tokio::spawn(async move {
        state
            .store
            .enqueue_job(KIND_CHECK_RUN_DISPATCH, payload)
            .await
            .inspect_err(|e| tracing::warn!(error = %e, "failed to enqueue check-run dispatch"))
            .ok();
    });
}

// ---------------------------------------------------------------------------
// Runner registration (admin)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct RegisterRunner {
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct RunnerResponse {
    #[serde(flatten)]
    pub runner: Runner,
    /// Bearer token for the report API. Shown once and never returned again.
    pub token: String,
}

/// `POST /api/orgs/{org}/repos/{repo}/runners`
pub async fn register_runner(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    Json(input): Json<RegisterRunner>,
) -> Result<Json<RunnerResponse>, AppError> {
    if input.name.trim().is_empty() {
        return Err(AppError::bad_request("runner name is required"));
    }
    let meta = state.store.get_repo(&org, &repo).await?;
    let token = generate_runner_token();
    let runner = state
        .store
        .register_runner(meta.id, input.name.trim(), &hash_token(&token), None)
        .await?;
    crate::audit::record(
        &state,
        Some(meta.id),
        None,
        "ci",
        "runner.registered",
        Some(&runner.name),
        None,
    )
    .await;
    Ok(Json(RunnerResponse { runner, token }))
}

/// `GET /api/orgs/{org}/repos/{repo}/runners`
pub async fn list_runners(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
) -> Result<Json<Vec<Runner>>, AppError> {
    let meta = state.store.get_repo(&org, &repo).await?;
    Ok(Json(state.store.list_runners(meta.id).await?))
}

/// `DELETE /api/orgs/{org}/repos/{repo}/runners/{id}`
pub async fn delete_runner(
    State(state): State<AppState>,
    Path((org, repo, runner_id)): Path<(String, String, Uuid)>,
) -> Result<StatusCode, AppError> {
    let meta = state.store.get_repo(&org, &repo).await?;
    state.store.delete_runner(meta.id, runner_id).await?;
    crate::audit::record(
        &state,
        Some(meta.id),
        None,
        "ci",
        "runner.deleted",
        Some(&runner_id.to_string()),
        None,
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Runner authentication (bearer token) for the report API.
// ---------------------------------------------------------------------------

/// Authenticate a runner from a bearer token (`Authorization: Bearer <t>` or
/// HTTP Basic password) and verify it is scoped to `repo_id`. Refreshes the
/// runner's `last_seen_at`.
async fn authenticate_runner(
    state: &AppState,
    headers: &HeaderMap,
    repo_id: Uuid,
) -> Result<Runner, AppError> {
    let token = bearer_token(headers)
        .ok_or_else(|| AppError::forbidden("runner bearer token required"))?;
    let runner = state
        .store
        .find_runner_by_token(&hash_token(&token))
        .await?
        .ok_or_else(|| AppError::forbidden("invalid runner token"))?;
    if runner.repo_id != repo_id {
        return Err(AppError::forbidden("runner is not scoped to this repository"));
    }
    // Liveness update is best-effort — never fail the report on it.
    let _ = state.store.touch_runner(runner.id).await;
    Ok(runner)
}

/// Extract a bearer token from `Authorization: Bearer <t>` or HTTP Basic
/// (`user:<t>` — the password field carries the token, git-style).
fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    if let Some(rest) = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
    {
        return Some(rest.trim().to_string());
    }
    if let Some(b64) = value
        .strip_prefix("Basic ")
        .or_else(|| value.strip_prefix("basic "))
    {
        use base64::Engine;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64.trim())
            .ok()?;
        let text = String::from_utf8(decoded).ok()?;
        let (_user, secret) = text.split_once(':')?;
        return Some(secret.to_string());
    }
    None
}

// ---------------------------------------------------------------------------
// Report API (runner-authenticated)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ReportCheckRun {
    pub name: String,
    pub head_sha: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub external_id: Option<String>,
    #[serde(default)]
    pub details_url: Option<String>,
    #[serde(default)]
    pub output_title: Option<String>,
    #[serde(default)]
    pub output_summary: Option<String>,
}

/// `POST /api/orgs/{org}/repos/{repo}/check-runs` — a runner creates a check run.
pub async fn create_check_run(
    State(state): State<AppState>,
    Path((org, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Json(input): Json<ReportCheckRun>,
) -> Result<Json<CheckRun>, AppError> {
    let meta = state.store.get_repo(&org, &repo).await?;
    let runner = authenticate_runner(&state, &headers, meta.id).await?;

    let status = parse_status(input.status.as_deref())?;
    let conclusion = parse_conclusion(input.conclusion.as_deref())?;
    if status == CheckRunStatus::Completed && conclusion.is_none() {
        return Err(AppError::bad_request(
            "a completed check run requires a conclusion",
        ));
    }
    if input.name.trim().is_empty() {
        return Err(AppError::bad_request("check run name is required"));
    }

    let run = state
        .store
        .create_check_run(
            meta.id,
            NewCheckRun {
                runner_id: Some(runner.id),
                head_sha: input.head_sha.trim(),
                name: input.name.trim(),
                external_id: input.external_id.as_deref(),
                status,
                conclusion,
                details_url: input.details_url.as_deref(),
                output_title: input.output_title.as_deref(),
                output_summary: input.output_summary.as_deref(),
            },
        )
        .await?;
    Ok(Json(run))
}

#[derive(Debug, Deserialize)]
pub struct UpdateCheckRun {
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub conclusion: Option<String>,
    #[serde(default)]
    pub details_url: Option<String>,
    #[serde(default)]
    pub output_title: Option<String>,
    #[serde(default)]
    pub output_summary: Option<String>,
}

/// `PATCH /api/orgs/{org}/repos/{repo}/check-runs/{id}` — advance a check run.
pub async fn update_check_run(
    State(state): State<AppState>,
    Path((org, repo, check_run_id)): Path<(String, String, Uuid)>,
    headers: HeaderMap,
    Json(input): Json<UpdateCheckRun>,
) -> Result<Json<CheckRun>, AppError> {
    let meta = state.store.get_repo(&org, &repo).await?;
    let _runner = authenticate_runner(&state, &headers, meta.id).await?;

    let status = parse_status(input.status.as_deref())?;
    let conclusion = parse_conclusion(input.conclusion.as_deref())?;
    // A caller that sets status without a value keeps the prior status (None).
    let status_opt = input.status.as_deref().map(|_| status);
    if matches!(status_opt, Some(CheckRunStatus::Completed)) && conclusion.is_none() {
        return Err(AppError::bad_request(
            "a completed check run requires a conclusion",
        ));
    }

    let run = state
        .store
        .update_check_run(
            meta.id,
            check_run_id,
            CheckRunPatch {
                status: status_opt,
                conclusion,
                details_url: input.details_url.as_deref(),
                output_title: input.output_title.as_deref(),
                output_summary: input.output_summary.as_deref(),
            },
        )
        .await?;
    Ok(Json(run))
}

// ---------------------------------------------------------------------------
// Surfacing check status on commits + PRs (public read)
// ---------------------------------------------------------------------------

/// `GET /api/orgs/{org}/repos/{repo}/commits/{sha}/check-runs`
pub async fn list_commit_check_runs(
    State(state): State<AppState>,
    Path((org, repo, sha)): Path<(String, String, String)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let meta = state.store.get_repo(&org, &repo).await?;
    let runs = state.store.latest_check_runs(meta.id, &sha).await?;
    Ok(Json(check_runs_envelope(&sha, runs)))
}

/// `GET /api/orgs/{org}/repos/{repo}/pulls/{number}/check-runs`
pub async fn list_pr_check_runs(
    State(state): State<AppState>,
    Path((org, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<serde_json::Value>, AppError> {
    let meta = state.store.get_repo(&org, &repo).await?;
    let pr = state.store.get_pr(meta.id, number).await?;
    let head = pr
        .head_sha
        .clone()
        .ok_or_else(|| AppError::not_found("pull request has no head commit"))?;
    let runs = state.store.latest_check_runs(meta.id, &head).await?;
    Ok(Json(check_runs_envelope(&head, runs)))
}

/// Combined roll-up + the per-name latest runs, GitHub-style.
fn check_runs_envelope(sha: &str, runs: Vec<CheckRun>) -> serde_json::Value {
    let combined = combined_state(&runs);
    json!({ "sha": sha, "state": combined, "total": runs.len(), "check_runs": runs })
}

/// Combined state: any incomplete run → "pending"; any completed run whose
/// conclusion is not passing → "failure"; otherwise (all passing) → "success".
/// No runs at all → "pending".
fn combined_state(runs: &[CheckRun]) -> &'static str {
    if runs.is_empty() {
        return "pending";
    }
    if runs.iter().any(|r| r.status != CheckRunStatus::Completed) {
        return "pending";
    }
    let all_pass = runs
        .iter()
        .all(|r| r.conclusion.map(|c| c.is_passing()).unwrap_or(false));
    if all_pass {
        "success"
    } else {
        "failure"
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse an optional status string, defaulting to `queued`.
fn parse_status(value: Option<&str>) -> Result<CheckRunStatus, AppError> {
    match value {
        None => Ok(CheckRunStatus::Queued),
        Some(s) => CheckRunStatus::parse(s)
            .ok_or_else(|| AppError::bad_request(format!("invalid check run status: {s}"))),
    }
}

/// Parse an optional conclusion string.
fn parse_conclusion(value: Option<&str>) -> Result<Option<CheckConclusion>, AppError> {
    match value {
        None => Ok(None),
        Some(s) => CheckConclusion::parse(s)
            .map(Some)
            .ok_or_else(|| AppError::bad_request(format!("invalid check run conclusion: {s}"))),
    }
}
