//! CI commit statuses: external checks report a `state` for a `context` on a
//! commit sha. The **latest** status per `(sha, context)` is authoritative, so a
//! required-status merge gate reads the most recent report per context.

use crate::error::{CoreError, Result};
use crate::store::PgStore;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

/// A CI check state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckState {
    Pending,
    Success,
    Failure,
    Error,
}

impl CheckState {
    pub fn as_str(&self) -> &'static str {
        match self {
            CheckState::Pending => "pending",
            CheckState::Success => "success",
            CheckState::Failure => "failure",
            CheckState::Error => "error",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "pending" => Some(CheckState::Pending),
            "success" => Some(CheckState::Success),
            "failure" => Some(CheckState::Failure),
            "error" => Some(CheckState::Error),
            _ => None,
        }
    }
}

/// A commit status record.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommitStatus {
    pub id: Uuid,
    pub sha: String,
    pub context: String,
    pub state: CheckState,
    pub target_url: Option<String>,
    pub description: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Input to set a status.
#[derive(Clone, Debug, Deserialize)]
pub struct NewStatus {
    pub context: String,
    pub state: String,
    #[serde(default)]
    pub target_url: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

fn status_from_row(row: &sqlx::postgres::PgRow) -> CommitStatus {
    CommitStatus {
        id: row.get("id"),
        sha: row.get("sha"),
        context: row.get("context"),
        state: CheckState::parse(row.get::<String, _>("state").as_str())
            .unwrap_or(CheckState::Error),
        target_url: row.get("target_url"),
        description: row.get("description"),
        created_at: row.get("created_at"),
    }
}

/// CI status persistence.
#[allow(async_fn_in_trait)]
pub trait StatusStore {
    async fn set_status(
        &self,
        repo_id: Uuid,
        sha: &str,
        input: &NewStatus,
        creator_id: Option<Uuid>,
    ) -> Result<CommitStatus>;
    /// The latest status per context for a commit.
    async fn latest_statuses(&self, repo_id: Uuid, sha: &str) -> Result<Vec<CommitStatus>>;
    /// Whether every `required` context has a latest state of `success`.
    async fn required_checks_pass(
        &self,
        repo_id: Uuid,
        sha: &str,
        required: &[String],
    ) -> Result<bool>;
}

impl StatusStore for PgStore {
    async fn set_status(
        &self,
        repo_id: Uuid,
        sha: &str,
        input: &NewStatus,
        creator_id: Option<Uuid>,
    ) -> Result<CommitStatus> {
        let state = CheckState::parse(&input.state)
            .ok_or_else(|| CoreError::InvalidName { kind: "status state", value: input.state.clone() })?;
        let row = sqlx::query(
            "INSERT INTO commit_statuses (repo_id, sha, context, state, target_url, description, creator_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             RETURNING id, sha, context, state, target_url, description, created_at",
        )
        .bind(repo_id)
        .bind(sha)
        .bind(&input.context)
        .bind(state.as_str())
        .bind(&input.target_url)
        .bind(&input.description)
        .bind(creator_id)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("status", &input.context, e))?;
        Ok(status_from_row(&row))
    }

    async fn latest_statuses(&self, repo_id: Uuid, sha: &str) -> Result<Vec<CommitStatus>> {
        // One row per context: the most recent report.
        let rows = sqlx::query(
            "SELECT DISTINCT ON (context) id, sha, context, state, target_url, description, created_at \
             FROM commit_statuses WHERE repo_id = $1 AND sha = $2 \
             ORDER BY context, created_at DESC",
        )
        .bind(repo_id)
        .bind(sha)
        .fetch_all(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("status", sha, e))?;
        Ok(rows.iter().map(status_from_row).collect())
    }

    async fn required_checks_pass(
        &self,
        repo_id: Uuid,
        sha: &str,
        required: &[String],
    ) -> Result<bool> {
        if required.is_empty() {
            return Ok(true);
        }
        let latest = self.latest_statuses(repo_id, sha).await?;
        for context in required {
            let ok = latest
                .iter()
                .any(|s| &s.context == context && s.state == CheckState::Success);
            if !ok {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
