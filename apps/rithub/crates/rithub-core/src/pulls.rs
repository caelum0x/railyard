//! Pull requests: domain model + Postgres-backed store. Mergeability, the diff,
//! and the merge itself are computed by the git layer (`rithub-git`) over `rit`;
//! this module owns only the PR metadata + state machine.

use crate::error::{CoreError, Result};
use crate::numbering::next_repo_number;
use crate::store::PgStore;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

/// The PR lifecycle state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PrState {
    Open,
    Closed,
    Merged,
}

impl PrState {
    pub fn as_str(&self) -> &'static str {
        match self {
            PrState::Open => "open",
            PrState::Closed => "closed",
            PrState::Merged => "merged",
        }
    }
    pub fn parse(value: &str) -> Self {
        match value {
            "merged" => PrState::Merged,
            "closed" => PrState::Closed,
            _ => PrState::Open,
        }
    }
}

/// A pull request (same-repository head→base to start).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PullRequest {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub number: i32,
    pub title: String,
    pub body: Option<String>,
    pub state: PrState,
    pub author_id: Option<Uuid>,
    pub base_branch: String,
    pub head_branch: String,
    pub base_sha: Option<String>,
    pub head_sha: Option<String>,
    /// Source repo for a cross-repo (fork) PR; `None` for a same-repo PR.
    pub head_repo_id: Option<Uuid>,
    pub merge_commit_sha: Option<String>,
    pub milestone_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub merged_at: Option<OffsetDateTime>,
    pub merged_by: Option<Uuid>,
}

/// Input to update a pull request. All fields optional; absent fields unchanged.
#[derive(Clone, Debug, Deserialize)]
pub struct UpdatePr {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    /// New base branch (changes the merge target).
    #[serde(default)]
    pub base: Option<String>,
    /// `"open"` to reopen a closed PR, `"closed"` to close an open one.
    #[serde(default)]
    pub state: Option<String>,
    /// `true` means `milestone_id` should be written (even if NULL).
    #[serde(skip)]
    pub set_milestone: bool,
    #[serde(skip)]
    pub milestone_id: Option<Uuid>,
}

/// A comment on a pull request.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PrComment {
    pub id: Uuid,
    pub pr_id: Uuid,
    pub author_id: Option<Uuid>,
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Input to open a pull request.
#[derive(Clone, Debug, Deserialize)]
pub struct NewPullRequest {
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    pub head: String,
    pub base: String,
}

const PR_COLUMNS: &str = "id, repo_id, number, title, body, state, author_id, base_branch, \
    head_branch, base_sha, head_sha, head_repo_id, merge_commit_sha, milestone_id, \
    created_at, updated_at, merged_at, merged_by";

fn pr_from_row(row: &sqlx::postgres::PgRow) -> PullRequest {
    PullRequest {
        id: row.get("id"),
        repo_id: row.get("repo_id"),
        number: row.get("number"),
        title: row.get("title"),
        body: row.get("body"),
        state: PrState::parse(row.get::<String, _>("state").as_str()),
        author_id: row.get("author_id"),
        base_branch: row.get("base_branch"),
        head_branch: row.get("head_branch"),
        base_sha: row.get("base_sha"),
        head_sha: row.get("head_sha"),
        head_repo_id: row.get("head_repo_id"),
        merge_commit_sha: row.get("merge_commit_sha"),
        milestone_id: row.get("milestone_id"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        merged_at: row.get("merged_at"),
        merged_by: row.get("merged_by"),
    }
}

/// Pull request persistence.
#[allow(async_fn_in_trait)]
pub trait PullRequestStore {
    #[allow(clippy::too_many_arguments)]
    async fn create_pr(
        &self,
        repo_id: Uuid,
        author_id: Option<Uuid>,
        input: &NewPullRequest,
        base_sha: Option<&str>,
        head_sha: Option<&str>,
        head_repo_id: Option<Uuid>,
    ) -> Result<PullRequest>;
    async fn list_prs(&self, repo_id: Uuid, state: Option<PrState>) -> Result<Vec<PullRequest>>;
    async fn get_pr(&self, repo_id: Uuid, number: i32) -> Result<PullRequest>;
    /// Close a PR without merging.
    async fn close_pr(&self, repo_id: Uuid, number: i32) -> Result<PullRequest>;
    /// Reopen a previously closed (not merged) PR.
    async fn reopen_pr(&self, repo_id: Uuid, number: i32) -> Result<PullRequest>;
    /// Update mutable PR metadata (title, body, base branch, milestone).
    async fn update_pr(&self, repo_id: Uuid, number: i32, input: &UpdatePr) -> Result<PullRequest>;
    /// Mark a PR merged, recording the merge commit and merger.
    async fn mark_merged(
        &self,
        repo_id: Uuid,
        number: i32,
        merge_commit_sha: &str,
        merged_by: Option<Uuid>,
    ) -> Result<PullRequest>;

    async fn add_pr_comment(
        &self,
        pr_id: Uuid,
        author_id: Option<Uuid>,
        body: &str,
    ) -> Result<PrComment>;
    async fn list_pr_comments(&self, pr_id: Uuid) -> Result<Vec<PrComment>>;
}

impl PullRequestStore for PgStore {
    async fn create_pr(
        &self,
        repo_id: Uuid,
        author_id: Option<Uuid>,
        input: &NewPullRequest,
        base_sha: Option<&str>,
        head_sha: Option<&str>,
        head_repo_id: Option<Uuid>,
    ) -> Result<PullRequest> {
        let mut tx = self.pool().begin().await.map_err(map_db)?;
        let number = next_repo_number(&mut tx, repo_id, "pull_requests").await?;
        let row = sqlx::query(&format!(
            "INSERT INTO pull_requests \
             (repo_id, number, title, body, author_id, base_branch, head_branch, base_sha, \
              head_sha, head_repo_id) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING {PR_COLUMNS}"
        ))
        .bind(repo_id)
        .bind(number)
        .bind(&input.title)
        .bind(&input.body)
        .bind(author_id)
        .bind(&input.base)
        .bind(&input.head)
        .bind(base_sha)
        .bind(head_sha)
        .bind(head_repo_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        Ok(pr_from_row(&row))
    }

    async fn list_prs(&self, repo_id: Uuid, state: Option<PrState>) -> Result<Vec<PullRequest>> {
        let rows = match state {
            Some(state) => {
                sqlx::query(&format!(
                    "SELECT {PR_COLUMNS} FROM pull_requests WHERE repo_id = $1 AND state = $2 \
                     ORDER BY number DESC"
                ))
                .bind(repo_id)
                .bind(state.as_str())
                .fetch_all(self.pool())
                .await
            }
            None => {
                sqlx::query(&format!(
                    "SELECT {PR_COLUMNS} FROM pull_requests WHERE repo_id = $1 ORDER BY number DESC"
                ))
                .bind(repo_id)
                .fetch_all(self.pool())
                .await
            }
        }
        .map_err(map_db)?;
        Ok(rows.iter().map(pr_from_row).collect())
    }

    async fn get_pr(&self, repo_id: Uuid, number: i32) -> Result<PullRequest> {
        let row = sqlx::query(&format!(
            "SELECT {PR_COLUMNS} FROM pull_requests WHERE repo_id = $1 AND number = $2"
        ))
        .bind(repo_id)
        .bind(number)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("pull request", &number.to_string(), e))?;
        Ok(pr_from_row(&row))
    }

    async fn close_pr(&self, repo_id: Uuid, number: i32) -> Result<PullRequest> {
        let row = sqlx::query(&format!(
            "UPDATE pull_requests SET state = 'closed', updated_at = now() \
             WHERE repo_id = $1 AND number = $2 AND state = 'open' RETURNING {PR_COLUMNS}"
        ))
        .bind(repo_id)
        .bind(number)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("pull request", &number.to_string(), e))?;
        Ok(pr_from_row(&row))
    }

    async fn reopen_pr(&self, repo_id: Uuid, number: i32) -> Result<PullRequest> {
        let row = sqlx::query(&format!(
            "UPDATE pull_requests SET state = 'open', updated_at = now() \
             WHERE repo_id = $1 AND number = $2 AND state = 'closed' RETURNING {PR_COLUMNS}"
        ))
        .bind(repo_id)
        .bind(number)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("pull request", &number.to_string(), e))?;
        Ok(pr_from_row(&row))
    }

    async fn update_pr(&self, repo_id: Uuid, number: i32, input: &UpdatePr) -> Result<PullRequest> {
        let row = sqlx::query(&format!(
            "UPDATE pull_requests SET \
                title        = COALESCE($3, title), \
                body         = COALESCE($4, body), \
                base_branch  = COALESCE($5, base_branch), \
                milestone_id = CASE WHEN $6 THEN $7 ELSE milestone_id END, \
                updated_at   = now() \
             WHERE repo_id = $1 AND number = $2 \
             RETURNING {PR_COLUMNS}"
        ))
        .bind(repo_id)
        .bind(number)
        .bind(&input.title)
        .bind(&input.body)
        .bind(&input.base)
        .bind(input.set_milestone)
        .bind(input.milestone_id)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("pull request", &number.to_string(), e))?;
        Ok(pr_from_row(&row))
    }

    async fn mark_merged(
        &self,
        repo_id: Uuid,
        number: i32,
        merge_commit_sha: &str,
        merged_by: Option<Uuid>,
    ) -> Result<PullRequest> {
        let row = sqlx::query(&format!(
            "UPDATE pull_requests SET state = 'merged', merge_commit_sha = $3, \
                merged_at = now(), merged_by = $4, updated_at = now() \
             WHERE repo_id = $1 AND number = $2 AND state = 'open' RETURNING {PR_COLUMNS}"
        ))
        .bind(repo_id)
        .bind(number)
        .bind(merge_commit_sha)
        .bind(merged_by)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("pull request", &number.to_string(), e))?;
        Ok(pr_from_row(&row))
    }

    async fn add_pr_comment(
        &self,
        pr_id: Uuid,
        author_id: Option<Uuid>,
        body: &str,
    ) -> Result<PrComment> {
        let row = sqlx::query(
            "INSERT INTO pr_comments (pr_id, author_id, body) VALUES ($1, $2, $3) \
             RETURNING id, pr_id, author_id, body, created_at",
        )
        .bind(pr_id)
        .bind(author_id)
        .bind(body)
        .fetch_one(self.pool())
        .await
        .map_err(map_db)?;
        Ok(PrComment {
            id: row.get("id"),
            pr_id: row.get("pr_id"),
            author_id: row.get("author_id"),
            body: row.get("body"),
            created_at: row.get("created_at"),
        })
    }

    async fn list_pr_comments(&self, pr_id: Uuid) -> Result<Vec<PrComment>> {
        let rows = sqlx::query(
            "SELECT id, pr_id, author_id, body, created_at FROM pr_comments \
             WHERE pr_id = $1 ORDER BY created_at ASC",
        )
        .bind(pr_id)
        .fetch_all(self.pool())
        .await
        .map_err(map_db)?;
        Ok(rows
            .iter()
            .map(|row| PrComment {
                id: row.get("id"),
                pr_id: row.get("pr_id"),
                author_id: row.get("author_id"),
                body: row.get("body"),
                created_at: row.get("created_at"),
            })
            .collect())
    }
}

fn map_db(e: sqlx::Error) -> CoreError {
    CoreError::from_sqlx("pull request", "*", e)
}
