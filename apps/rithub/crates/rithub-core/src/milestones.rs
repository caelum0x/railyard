//! Milestones: domain model + Postgres-backed store.
//!
//! A milestone groups issues and pull requests under a shared goal with an
//! optional due date. Progress (open/closed issue counts) is computed on read
//! from a single LEFT JOIN rather than maintained incrementally, keeping the
//! write path simple.

use crate::error::{CoreError, Result};
use crate::numbering::next_repo_number;
use crate::store::PgStore;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

/// A repository milestone.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Milestone {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub number: i32,
    pub title: String,
    pub description: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub due_on: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub closed_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

/// A milestone with a live progress snapshot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MilestoneWithProgress {
    #[serde(flatten)]
    pub milestone: Milestone,
    /// Number of open issues currently assigned to this milestone.
    pub open_issues: i64,
    /// Number of closed issues currently assigned to this milestone.
    pub closed_issues: i64,
}

/// Input to create a milestone.
#[derive(Clone, Debug, Deserialize)]
pub struct NewMilestone {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    #[serde(with = "time::serde::rfc3339::option")]
    pub due_on: Option<OffsetDateTime>,
}

/// Input to update a milestone (all fields optional — only provided fields change).
#[derive(Clone, Debug, Deserialize)]
pub struct UpdateMilestone {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Pass `null` in JSON to clear the due date.
    #[serde(default)]
    #[serde(with = "time::serde::rfc3339::option")]
    pub due_on: Option<OffsetDateTime>,
    /// `"open"` to reopen, `"closed"` to close.
    #[serde(default)]
    pub state: Option<String>,
}

fn milestone_from_row(row: &sqlx::postgres::PgRow) -> Milestone {
    Milestone {
        id: row.get("id"),
        repo_id: row.get("repo_id"),
        number: row.get("number"),
        title: row.get("title"),
        description: row.get("description"),
        due_on: row.get("due_on"),
        closed_at: row.get("closed_at"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

/// Milestone persistence.
#[allow(async_fn_in_trait)]
pub trait MilestoneStore {
    async fn create_milestone(&self, repo_id: Uuid, input: &NewMilestone) -> Result<Milestone>;
    async fn list_milestones(&self, repo_id: Uuid) -> Result<Vec<MilestoneWithProgress>>;
    async fn get_milestone(
        &self,
        repo_id: Uuid,
        number: i32,
    ) -> Result<MilestoneWithProgress>;
    async fn update_milestone(
        &self,
        repo_id: Uuid,
        number: i32,
        input: &UpdateMilestone,
    ) -> Result<Milestone>;
    async fn delete_milestone(&self, repo_id: Uuid, number: i32) -> Result<()>;
    /// Resolve a milestone number to its UUID within a repo.
    async fn get_milestone_id(&self, repo_id: Uuid, number: i32) -> Result<Uuid>;
}

impl MilestoneStore for PgStore {
    async fn create_milestone(&self, repo_id: Uuid, input: &NewMilestone) -> Result<Milestone> {
        let mut tx = self.pool().begin().await.map_err(map_db)?;
        let number = next_repo_number(&mut tx, repo_id, "milestones").await?;
        let row = sqlx::query(
            "INSERT INTO milestones (repo_id, number, title, description, due_on) \
             VALUES ($1, $2, $3, $4, $5) \
             RETURNING id, repo_id, number, title, description, due_on, \
                       closed_at, created_at, updated_at",
        )
        .bind(repo_id)
        .bind(number)
        .bind(&input.title)
        .bind(&input.description)
        .bind(input.due_on)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        Ok(milestone_from_row(&row))
    }

    async fn list_milestones(&self, repo_id: Uuid) -> Result<Vec<MilestoneWithProgress>> {
        let rows = sqlx::query(
            "SELECT m.id, m.repo_id, m.number, m.title, m.description, m.due_on, \
                    m.closed_at, m.created_at, m.updated_at, \
                    COUNT(i.id) FILTER (WHERE i.state = 'open')   AS open_issues, \
                    COUNT(i.id) FILTER (WHERE i.state = 'closed') AS closed_issues \
             FROM milestones m \
             LEFT JOIN issues i ON i.milestone_id = m.id \
             WHERE m.repo_id = $1 \
             GROUP BY m.id \
             ORDER BY m.number ASC",
        )
        .bind(repo_id)
        .fetch_all(self.pool())
        .await
        .map_err(map_db)?;
        Ok(rows
            .iter()
            .map(|row| MilestoneWithProgress {
                milestone: milestone_from_row(row),
                open_issues: row.get("open_issues"),
                closed_issues: row.get("closed_issues"),
            })
            .collect())
    }

    async fn get_milestone(
        &self,
        repo_id: Uuid,
        number: i32,
    ) -> Result<MilestoneWithProgress> {
        let row = sqlx::query(
            "SELECT m.id, m.repo_id, m.number, m.title, m.description, m.due_on, \
                    m.closed_at, m.created_at, m.updated_at, \
                    COUNT(i.id) FILTER (WHERE i.state = 'open')   AS open_issues, \
                    COUNT(i.id) FILTER (WHERE i.state = 'closed') AS closed_issues \
             FROM milestones m \
             LEFT JOIN issues i ON i.milestone_id = m.id \
             WHERE m.repo_id = $1 AND m.number = $2 \
             GROUP BY m.id",
        )
        .bind(repo_id)
        .bind(number)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("milestone", &number.to_string(), e))?;
        Ok(MilestoneWithProgress {
            milestone: milestone_from_row(&row),
            open_issues: row.get("open_issues"),
            closed_issues: row.get("closed_issues"),
        })
    }

    async fn update_milestone(
        &self,
        repo_id: Uuid,
        number: i32,
        input: &UpdateMilestone,
    ) -> Result<Milestone> {
        // Resolve `state` to whether we should close or reopen.
        let (set_closed, closed_value): (bool, bool) = match input.state.as_deref() {
            Some("closed") => (true, true),
            Some("open") => (true, false),
            _ => (false, false),
        };
        let row = sqlx::query(
            "UPDATE milestones SET \
                title       = COALESCE($3, title), \
                description = COALESCE($4, description), \
                due_on      = COALESCE($5, due_on), \
                closed_at   = CASE WHEN $6 THEN (CASE WHEN $7 THEN now() ELSE NULL END) ELSE closed_at END, \
                updated_at  = now() \
             WHERE repo_id = $1 AND number = $2 \
             RETURNING id, repo_id, number, title, description, due_on, \
                       closed_at, created_at, updated_at",
        )
        .bind(repo_id)
        .bind(number)
        .bind(&input.title)
        .bind(&input.description)
        .bind(input.due_on)
        .bind(set_closed)
        .bind(closed_value)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("milestone", &number.to_string(), e))?;
        Ok(milestone_from_row(&row))
    }

    async fn delete_milestone(&self, repo_id: Uuid, number: i32) -> Result<()> {
        let result = sqlx::query(
            "DELETE FROM milestones WHERE repo_id = $1 AND number = $2",
        )
        .bind(repo_id)
        .bind(number)
        .execute(self.pool())
        .await
        .map_err(map_db)?;
        if result.rows_affected() == 0 {
            return Err(CoreError::NotFound {
                kind: "milestone",
                key: number.to_string(),
            });
        }
        Ok(())
    }

    async fn get_milestone_id(&self, repo_id: Uuid, number: i32) -> Result<Uuid> {
        let row = sqlx::query("SELECT id FROM milestones WHERE repo_id = $1 AND number = $2")
            .bind(repo_id)
            .bind(number)
            .fetch_one(self.pool())
            .await
            .map_err(|e| CoreError::from_sqlx("milestone", &number.to_string(), e))?;
        Ok(row.get("id"))
    }
}

fn map_db(e: sqlx::Error) -> CoreError {
    CoreError::from_sqlx("milestone", "*", e)
}
