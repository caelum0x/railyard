//! CI runners + check runs: the external-runner contract.
//!
//! A [`Runner`] is an out-of-band worker registered against a repository with a
//! bearer token (only the SHA-256 hash is stored). On push/PR the server
//! dispatches a `check_run` webhook event through the outbox; a listening runner
//! executes the job and reports progress by creating and updating [`CheckRun`]
//! rows through the authenticated report API.
//!
//! `CheckRun` is the GitHub-Checks-style status model: `queued → in_progress →
//! completed` with a typed [`CheckConclusion`]. It is richer than the simpler
//! `commit_statuses` ping (see `statuses.rs`) and coexists with it.

use crate::error::{CoreError, Result};
use crate::store::PgStore;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Status + conclusion enums
// ---------------------------------------------------------------------------

/// The lifecycle state of a check run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckRunStatus {
    Queued,
    InProgress,
    Completed,
}

impl CheckRunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            CheckRunStatus::Queued => "queued",
            CheckRunStatus::InProgress => "in_progress",
            CheckRunStatus::Completed => "completed",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(CheckRunStatus::Queued),
            "in_progress" => Some(CheckRunStatus::InProgress),
            "completed" => Some(CheckRunStatus::Completed),
            _ => None,
        }
    }
}

/// The terminal outcome of a completed check run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckConclusion {
    Success,
    Failure,
    Neutral,
    Cancelled,
    TimedOut,
    ActionRequired,
    Skipped,
}

impl CheckConclusion {
    pub fn as_str(&self) -> &'static str {
        match self {
            CheckConclusion::Success => "success",
            CheckConclusion::Failure => "failure",
            CheckConclusion::Neutral => "neutral",
            CheckConclusion::Cancelled => "cancelled",
            CheckConclusion::TimedOut => "timed_out",
            CheckConclusion::ActionRequired => "action_required",
            CheckConclusion::Skipped => "skipped",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "success" => Some(CheckConclusion::Success),
            "failure" => Some(CheckConclusion::Failure),
            "neutral" => Some(CheckConclusion::Neutral),
            "cancelled" => Some(CheckConclusion::Cancelled),
            "timed_out" => Some(CheckConclusion::TimedOut),
            "action_required" => Some(CheckConclusion::ActionRequired),
            "skipped" => Some(CheckConclusion::Skipped),
            _ => None,
        }
    }
    /// A conclusion that should count as "the check passed" for a merge gate.
    pub fn is_passing(&self) -> bool {
        matches!(
            self,
            CheckConclusion::Success | CheckConclusion::Neutral | CheckConclusion::Skipped
        )
    }
}

// ---------------------------------------------------------------------------
// Domain structs
// ---------------------------------------------------------------------------

/// A registered external CI runner. The token is never returned after creation.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Runner {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub name: String,
    pub created_by: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_seen_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

fn runner_from_row(row: &sqlx::postgres::PgRow) -> Runner {
    Runner {
        id: row.get("id"),
        repo_id: row.get("repo_id"),
        name: row.get("name"),
        created_by: row.get("created_by"),
        last_seen_at: row.get("last_seen_at"),
        created_at: row.get("created_at"),
    }
}

/// A check run: one named CI job against a commit.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CheckRun {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub runner_id: Option<Uuid>,
    pub head_sha: String,
    pub name: String,
    pub external_id: Option<String>,
    pub status: CheckRunStatus,
    pub conclusion: Option<CheckConclusion>,
    pub details_url: Option<String>,
    pub output_title: Option<String>,
    pub output_summary: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub started_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub completed_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

fn check_run_from_row(row: &sqlx::postgres::PgRow) -> CheckRun {
    let status: String = row.get("status");
    let conclusion: Option<String> = row.get("conclusion");
    CheckRun {
        id: row.get("id"),
        repo_id: row.get("repo_id"),
        runner_id: row.get("runner_id"),
        head_sha: row.get("head_sha"),
        name: row.get("name"),
        external_id: row.get("external_id"),
        status: CheckRunStatus::parse(&status).unwrap_or(CheckRunStatus::Queued),
        conclusion: conclusion.as_deref().and_then(CheckConclusion::parse),
        details_url: row.get("details_url"),
        output_title: row.get("output_title"),
        output_summary: row.get("output_summary"),
        started_at: row.get("started_at"),
        completed_at: row.get("completed_at"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

/// Fields a runner supplies when creating a check run.
#[derive(Clone, Debug)]
pub struct NewCheckRun<'a> {
    pub runner_id: Option<Uuid>,
    pub head_sha: &'a str,
    pub name: &'a str,
    pub external_id: Option<&'a str>,
    pub status: CheckRunStatus,
    pub conclusion: Option<CheckConclusion>,
    pub details_url: Option<&'a str>,
    pub output_title: Option<&'a str>,
    pub output_summary: Option<&'a str>,
}

/// Fields a runner may change when updating a check run.
#[derive(Clone, Debug, Default)]
pub struct CheckRunPatch<'a> {
    pub status: Option<CheckRunStatus>,
    pub conclusion: Option<CheckConclusion>,
    pub details_url: Option<&'a str>,
    pub output_title: Option<&'a str>,
    pub output_summary: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// Store trait
// ---------------------------------------------------------------------------

/// Persistence for CI runners and check runs.
#[allow(async_fn_in_trait)]
pub trait CiStore {
    /// Register a runner against a repo, storing only its token hash.
    async fn register_runner(
        &self,
        repo_id: Uuid,
        name: &str,
        token_hash: &str,
        created_by: Option<Uuid>,
    ) -> Result<Runner>;
    /// List a repo's runners, newest first.
    async fn list_runners(&self, repo_id: Uuid) -> Result<Vec<Runner>>;
    /// Delete a runner by id (scoped to its repo).
    async fn delete_runner(&self, repo_id: Uuid, runner_id: Uuid) -> Result<()>;
    /// Look up a runner by its token hash (repo-scoped bearer auth).
    async fn find_runner_by_token(&self, token_hash: &str) -> Result<Option<Runner>>;
    /// Record that a runner just authenticated (liveness).
    async fn touch_runner(&self, runner_id: Uuid) -> Result<()>;

    /// Create a check run.
    async fn create_check_run(&self, repo_id: Uuid, input: NewCheckRun<'_>) -> Result<CheckRun>;
    /// Update a check run's status/conclusion/output (scoped to its repo).
    async fn update_check_run(
        &self,
        repo_id: Uuid,
        check_run_id: Uuid,
        patch: CheckRunPatch<'_>,
    ) -> Result<CheckRun>;
    /// Fetch one check run by id (scoped to its repo).
    async fn get_check_run(&self, repo_id: Uuid, check_run_id: Uuid) -> Result<CheckRun>;
    /// All check runs for a commit, newest first.
    async fn list_check_runs(&self, repo_id: Uuid, head_sha: &str) -> Result<Vec<CheckRun>>;
    /// The latest check run per distinct name for a commit.
    async fn latest_check_runs(&self, repo_id: Uuid, head_sha: &str) -> Result<Vec<CheckRun>>;
}

impl CiStore for PgStore {
    async fn register_runner(
        &self,
        repo_id: Uuid,
        name: &str,
        token_hash: &str,
        created_by: Option<Uuid>,
    ) -> Result<Runner> {
        let row = sqlx::query(
            "INSERT INTO ci_runners (repo_id, name, token_hash, created_by) \
             VALUES ($1, $2, $3, $4) \
             RETURNING id, repo_id, name, created_by, last_seen_at, created_at",
        )
        .bind(repo_id)
        .bind(name)
        .bind(token_hash)
        .bind(created_by)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("runner", name, e))?;
        Ok(runner_from_row(&row))
    }

    async fn list_runners(&self, repo_id: Uuid) -> Result<Vec<Runner>> {
        let rows = sqlx::query(
            "SELECT id, repo_id, name, created_by, last_seen_at, created_at \
             FROM ci_runners WHERE repo_id = $1 ORDER BY created_at DESC",
        )
        .bind(repo_id)
        .fetch_all(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("runner", "*", e))?;
        Ok(rows.iter().map(runner_from_row).collect())
    }

    async fn delete_runner(&self, repo_id: Uuid, runner_id: Uuid) -> Result<()> {
        let result = sqlx::query("DELETE FROM ci_runners WHERE repo_id = $1 AND id = $2")
            .bind(repo_id)
            .bind(runner_id)
            .execute(self.pool())
            .await
            .map_err(|e| CoreError::from_sqlx("runner", &runner_id.to_string(), e))?;
        if result.rows_affected() == 0 {
            return Err(CoreError::NotFound {
                kind: "runner",
                key: runner_id.to_string(),
            });
        }
        Ok(())
    }

    async fn find_runner_by_token(&self, token_hash: &str) -> Result<Option<Runner>> {
        let row = sqlx::query(
            "SELECT id, repo_id, name, created_by, last_seen_at, created_at \
             FROM ci_runners WHERE token_hash = $1",
        )
        .bind(token_hash)
        .fetch_optional(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("runner", "*", e))?;
        Ok(row.as_ref().map(runner_from_row))
    }

    async fn touch_runner(&self, runner_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE ci_runners SET last_seen_at = now() WHERE id = $1")
            .bind(runner_id)
            .execute(self.pool())
            .await
            .map_err(|e| CoreError::from_sqlx("runner", &runner_id.to_string(), e))?;
        Ok(())
    }

    async fn create_check_run(&self, repo_id: Uuid, input: NewCheckRun<'_>) -> Result<CheckRun> {
        // started_at/completed_at are derived from the initial status so timing
        // is consistent no matter which state the runner opens the run in.
        let row = sqlx::query(
            "INSERT INTO check_runs \
                (repo_id, runner_id, head_sha, name, external_id, status, conclusion, \
                 details_url, output_title, output_summary, started_at, completed_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, \
                 CASE WHEN $6 IN ('in_progress', 'completed') THEN now() END, \
                 CASE WHEN $6 = 'completed' THEN now() END) \
             RETURNING id, repo_id, runner_id, head_sha, name, external_id, status, conclusion, \
                 details_url, output_title, output_summary, started_at, completed_at, \
                 created_at, updated_at",
        )
        .bind(repo_id)
        .bind(input.runner_id)
        .bind(input.head_sha)
        .bind(input.name)
        .bind(input.external_id)
        .bind(input.status.as_str())
        .bind(input.conclusion.map(|c| c.as_str()))
        .bind(input.details_url)
        .bind(input.output_title)
        .bind(input.output_summary)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("check_run", input.name, e))?;
        Ok(check_run_from_row(&row))
    }

    async fn update_check_run(
        &self,
        repo_id: Uuid,
        check_run_id: Uuid,
        patch: CheckRunPatch<'_>,
    ) -> Result<CheckRun> {
        // COALESCE keeps prior values when a field is omitted. started_at is
        // stamped the first time the run leaves 'queued'; completed_at is
        // stamped when it reaches 'completed'.
        let row = sqlx::query(
            "UPDATE check_runs SET \
                status       = COALESCE($3, status), \
                conclusion   = COALESCE($4, conclusion), \
                details_url  = COALESCE($5, details_url), \
                output_title = COALESCE($6, output_title), \
                output_summary = COALESCE($7, output_summary), \
                started_at   = COALESCE(started_at, \
                                   CASE WHEN $3 IN ('in_progress', 'completed') THEN now() END), \
                completed_at = CASE WHEN $3 = 'completed' THEN now() ELSE completed_at END, \
                updated_at   = now() \
             WHERE repo_id = $1 AND id = $2 \
             RETURNING id, repo_id, runner_id, head_sha, name, external_id, status, conclusion, \
                 details_url, output_title, output_summary, started_at, completed_at, \
                 created_at, updated_at",
        )
        .bind(repo_id)
        .bind(check_run_id)
        .bind(patch.status.map(|s| s.as_str()))
        .bind(patch.conclusion.map(|c| c.as_str()))
        .bind(patch.details_url)
        .bind(patch.output_title)
        .bind(patch.output_summary)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("check_run", &check_run_id.to_string(), e))?;
        Ok(check_run_from_row(&row))
    }

    async fn get_check_run(&self, repo_id: Uuid, check_run_id: Uuid) -> Result<CheckRun> {
        let row = sqlx::query(
            "SELECT id, repo_id, runner_id, head_sha, name, external_id, status, conclusion, \
                 details_url, output_title, output_summary, started_at, completed_at, \
                 created_at, updated_at \
             FROM check_runs WHERE repo_id = $1 AND id = $2",
        )
        .bind(repo_id)
        .bind(check_run_id)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("check_run", &check_run_id.to_string(), e))?;
        Ok(check_run_from_row(&row))
    }

    async fn list_check_runs(&self, repo_id: Uuid, head_sha: &str) -> Result<Vec<CheckRun>> {
        let rows = sqlx::query(
            "SELECT id, repo_id, runner_id, head_sha, name, external_id, status, conclusion, \
                 details_url, output_title, output_summary, started_at, completed_at, \
                 created_at, updated_at \
             FROM check_runs WHERE repo_id = $1 AND head_sha = $2 \
             ORDER BY created_at DESC",
        )
        .bind(repo_id)
        .bind(head_sha)
        .fetch_all(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("check_run", head_sha, e))?;
        Ok(rows.iter().map(check_run_from_row).collect())
    }

    async fn latest_check_runs(&self, repo_id: Uuid, head_sha: &str) -> Result<Vec<CheckRun>> {
        // One row per check name: the most recent report.
        let rows = sqlx::query(
            "SELECT DISTINCT ON (name) \
                 id, repo_id, runner_id, head_sha, name, external_id, status, conclusion, \
                 details_url, output_title, output_summary, started_at, completed_at, \
                 created_at, updated_at \
             FROM check_runs WHERE repo_id = $1 AND head_sha = $2 \
             ORDER BY name, created_at DESC",
        )
        .bind(repo_id)
        .bind(head_sha)
        .fetch_all(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("check_run", head_sha, e))?;
        Ok(rows.iter().map(check_run_from_row).collect())
    }
}
