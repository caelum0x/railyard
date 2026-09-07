//! Pull-request reviews + inline threads + a basic protected-branch rule.
//!
//! The protected-branch gate is intentionally minimal: a branch may require ≥N
//! approvals before a PR targeting it can merge. `count_current_approvals`
//! counts the **latest** review state per reviewer (a later change-request
//! supersedes an earlier approval), which is what a merge gate must consider.
//! The PR **author** is excluded — like GitHub, a self-approval never satisfies
//! the required-approvals gate.

use crate::error::{CoreError, Result};
use crate::store::PgStore;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

/// A review verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewState {
    Approved,
    ChangesRequested,
    Commented,
}

impl ReviewState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ReviewState::Approved => "approved",
            ReviewState::ChangesRequested => "changes_requested",
            ReviewState::Commented => "commented",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "approved" => Some(ReviewState::Approved),
            "changes_requested" => Some(ReviewState::ChangesRequested),
            "commented" => Some(ReviewState::Commented),
            _ => None,
        }
    }
}

/// A submitted review.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Review {
    pub id: Uuid,
    pub pr_id: Uuid,
    pub reviewer_id: Option<Uuid>,
    pub state: ReviewState,
    pub body: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// An inline review-thread comment.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReviewComment {
    pub id: Uuid,
    pub review_id: Uuid,
    pub pr_id: Uuid,
    pub path: String,
    pub line: Option<i32>,
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A protected-branch rule.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BranchProtection {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub branch: String,
    pub required_approvals: i32,
    /// Whether direct pushes (bypassing a PR) are blocked for non-admins.
    pub block_direct_push: bool,
    /// CI contexts that must be `success` on the PR head before merge.
    pub required_checks: Vec<String>,
}

/// An inline thread comment to attach to a review.
#[derive(Clone, Debug, Deserialize)]
pub struct NewReviewComment {
    pub path: String,
    #[serde(default)]
    pub line: Option<i32>,
    pub body: String,
}

fn review_from_row(row: &sqlx::postgres::PgRow) -> Review {
    Review {
        id: row.get("id"),
        pr_id: row.get("pr_id"),
        reviewer_id: row.get("reviewer_id"),
        state: ReviewState::parse(row.get::<String, _>("state").as_str())
            .unwrap_or(ReviewState::Commented),
        body: row.get("body"),
        created_at: row.get("created_at"),
    }
}

/// Review + protected-branch persistence.
#[allow(async_fn_in_trait)]
pub trait ReviewStore {
    async fn create_review(
        &self,
        pr_id: Uuid,
        reviewer_id: Option<Uuid>,
        state: ReviewState,
        body: Option<&str>,
        comments: &[NewReviewComment],
    ) -> Result<Review>;
    async fn list_reviews(&self, pr_id: Uuid) -> Result<Vec<Review>>;
    async fn list_review_comments(&self, pr_id: Uuid) -> Result<Vec<ReviewComment>>;

    /// Number of reviewers whose **latest** review approves the PR, excluding
    /// the PR's own author (`author_id`) so a self-approval never counts.
    async fn count_current_approvals(
        &self,
        pr_id: Uuid,
        author_id: Option<Uuid>,
    ) -> Result<i64>;

    async fn set_branch_protection(
        &self,
        repo_id: Uuid,
        branch: &str,
        required_approvals: i32,
        block_direct_push: bool,
        required_checks: &[String],
    ) -> Result<BranchProtection>;
    async fn get_branch_protection(
        &self,
        repo_id: Uuid,
        branch: &str,
    ) -> Result<Option<BranchProtection>>;
    /// Branch names in a repo that block direct pushes.
    async fn list_push_protected_branches(&self, repo_id: Uuid) -> Result<Vec<String>>;
}

impl ReviewStore for PgStore {
    async fn create_review(
        &self,
        pr_id: Uuid,
        reviewer_id: Option<Uuid>,
        state: ReviewState,
        body: Option<&str>,
        comments: &[NewReviewComment],
    ) -> Result<Review> {
        let mut tx = self.pool().begin().await.map_err(map_db)?;
        let row = sqlx::query(
            "INSERT INTO reviews (pr_id, reviewer_id, state, body) VALUES ($1, $2, $3, $4) \
             RETURNING id, pr_id, reviewer_id, state, body, created_at",
        )
        .bind(pr_id)
        .bind(reviewer_id)
        .bind(state.as_str())
        .bind(body)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;
        let review = review_from_row(&row);
        for comment in comments {
            sqlx::query(
                "INSERT INTO review_comments (review_id, pr_id, path, line, body) \
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(review.id)
            .bind(pr_id)
            .bind(&comment.path)
            .bind(comment.line)
            .bind(&comment.body)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        }
        tx.commit().await.map_err(map_db)?;
        Ok(review)
    }

    async fn list_reviews(&self, pr_id: Uuid) -> Result<Vec<Review>> {
        let rows = sqlx::query(
            "SELECT id, pr_id, reviewer_id, state, body, created_at FROM reviews \
             WHERE pr_id = $1 ORDER BY created_at ASC",
        )
        .bind(pr_id)
        .fetch_all(self.pool())
        .await
        .map_err(map_db)?;
        Ok(rows.iter().map(review_from_row).collect())
    }

    async fn list_review_comments(&self, pr_id: Uuid) -> Result<Vec<ReviewComment>> {
        let rows = sqlx::query(
            "SELECT id, review_id, pr_id, path, line, body, created_at FROM review_comments \
             WHERE pr_id = $1 ORDER BY created_at ASC",
        )
        .bind(pr_id)
        .fetch_all(self.pool())
        .await
        .map_err(map_db)?;
        Ok(rows
            .iter()
            .map(|row| ReviewComment {
                id: row.get("id"),
                review_id: row.get("review_id"),
                pr_id: row.get("pr_id"),
                path: row.get("path"),
                line: row.get("line"),
                body: row.get("body"),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    async fn count_current_approvals(
        &self,
        pr_id: Uuid,
        author_id: Option<Uuid>,
    ) -> Result<i64> {
        // For each reviewer, take their most recent review; count those that are
        // 'approved'. A reviewer who later requested changes does not count. The
        // PR author is excluded (`IS DISTINCT FROM` treats a NULL author as
        // "no one to exclude", so anonymous-authored PRs are unaffected).
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM ( \
               SELECT DISTINCT ON (reviewer_id) state \
               FROM reviews \
               WHERE pr_id = $1 AND reviewer_id IS NOT NULL \
                 AND reviewer_id IS DISTINCT FROM $2 \
               ORDER BY reviewer_id, created_at DESC \
             ) latest WHERE state = 'approved'",
        )
        .bind(pr_id)
        .bind(author_id)
        .fetch_one(self.pool())
        .await
        .map_err(map_db)?;
        Ok(count)
    }

    async fn set_branch_protection(
        &self,
        repo_id: Uuid,
        branch: &str,
        required_approvals: i32,
        block_direct_push: bool,
        required_checks: &[String],
    ) -> Result<BranchProtection> {
        let row = sqlx::query(
            "INSERT INTO branch_protections \
                (repo_id, branch, required_approvals, block_direct_push, required_checks) \
             VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (repo_id, branch) DO UPDATE SET \
                required_approvals = EXCLUDED.required_approvals, \
                block_direct_push = EXCLUDED.block_direct_push, \
                required_checks = EXCLUDED.required_checks \
             RETURNING id, repo_id, branch, required_approvals, block_direct_push, required_checks",
        )
        .bind(repo_id)
        .bind(branch)
        .bind(required_approvals)
        .bind(block_direct_push)
        .bind(required_checks)
        .fetch_one(self.pool())
        .await
        .map_err(map_db)?;
        Ok(protection_from_row(&row))
    }

    async fn get_branch_protection(
        &self,
        repo_id: Uuid,
        branch: &str,
    ) -> Result<Option<BranchProtection>> {
        let row = sqlx::query(
            "SELECT id, repo_id, branch, required_approvals, block_direct_push, required_checks \
             FROM branch_protections WHERE repo_id = $1 AND branch = $2",
        )
        .bind(repo_id)
        .bind(branch)
        .fetch_optional(self.pool())
        .await
        .map_err(map_db)?;
        Ok(row.as_ref().map(protection_from_row))
    }

    async fn list_push_protected_branches(&self, repo_id: Uuid) -> Result<Vec<String>> {
        let rows = sqlx::query_scalar::<_, String>(
            "SELECT branch FROM branch_protections WHERE repo_id = $1 AND block_direct_push",
        )
        .bind(repo_id)
        .fetch_all(self.pool())
        .await
        .map_err(map_db)?;
        Ok(rows)
    }
}

fn protection_from_row(row: &sqlx::postgres::PgRow) -> BranchProtection {
    BranchProtection {
        id: row.get("id"),
        repo_id: row.get("repo_id"),
        branch: row.get("branch"),
        required_approvals: row.get("required_approvals"),
        block_direct_push: row.get("block_direct_push"),
        required_checks: row.get("required_checks"),
    }
}

fn map_db(e: sqlx::Error) -> CoreError {
    CoreError::from_sqlx("review", "*", e)
}
