//! Issues: domain model + Postgres-backed store (a focused [`IssueStore`] trait
//! implemented for [`crate::PgStore`]). Kept in its own module so the store layer
//! stays high-cohesion instead of one monolithic trait.

use crate::error::{CoreError, Result};
use crate::numbering::next_repo_number;
use crate::store::PgStore;
use crate::User;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

/// Open/closed state of an issue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IssueState {
    Open,
    Closed,
}

impl IssueState {
    pub fn as_str(&self) -> &'static str {
        match self {
            IssueState::Open => "open",
            IssueState::Closed => "closed",
        }
    }
    pub fn parse(value: &str) -> Self {
        match value {
            "closed" => IssueState::Closed,
            _ => IssueState::Open,
        }
    }
}

/// An issue.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Issue {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub number: i32,
    pub title: String,
    pub body: Option<String>,
    pub state: IssueState,
    pub author_id: Option<Uuid>,
    pub milestone_id: Option<Uuid>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub closed_at: Option<OffsetDateTime>,
}

/// Input to update an issue. All fields are optional; absent fields are unchanged.
#[derive(Clone, Debug, Deserialize)]
pub struct UpdateIssue {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    /// `true` means the `milestone_id` parameter should be written (even if NULL).
    #[serde(skip)]
    pub set_milestone: bool,
    /// New milestone FK, or `None` to clear.
    #[serde(skip)]
    pub milestone_id: Option<Uuid>,
}

/// A comment on an issue.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IssueComment {
    pub id: Uuid,
    pub issue_id: Uuid,
    pub author_id: Option<Uuid>,
    pub body: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// A repository label.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Label {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub name: String,
    pub color: String,
}

/// Input to open an issue.
#[derive(Clone, Debug, Deserialize)]
pub struct NewIssue {
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
}

fn issue_from_row(row: &sqlx::postgres::PgRow) -> Issue {
    Issue {
        id: row.get("id"),
        repo_id: row.get("repo_id"),
        number: row.get("number"),
        title: row.get("title"),
        body: row.get("body"),
        state: IssueState::parse(row.get::<String, _>("state").as_str()),
        author_id: row.get("author_id"),
        milestone_id: row.get("milestone_id"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
        closed_at: row.get("closed_at"),
    }
}

/// Issue persistence.
#[allow(async_fn_in_trait)]
pub trait IssueStore {
    async fn create_issue(
        &self,
        repo_id: Uuid,
        author_id: Option<Uuid>,
        input: &NewIssue,
    ) -> Result<Issue>;
    async fn list_issues(&self, repo_id: Uuid, state: Option<IssueState>) -> Result<Vec<Issue>>;
    async fn get_issue(&self, repo_id: Uuid, number: i32) -> Result<Issue>;
    /// Move an issue to a new state, stamping `closed_at` appropriately.
    async fn set_issue_state(&self, repo_id: Uuid, number: i32, state: IssueState)
        -> Result<Issue>;
    /// Update mutable metadata fields on an issue (title, body, milestone).
    async fn update_issue(
        &self,
        repo_id: Uuid,
        number: i32,
        input: &UpdateIssue,
    ) -> Result<Issue>;

    async fn add_issue_comment(
        &self,
        issue_id: Uuid,
        author_id: Option<Uuid>,
        body: &str,
    ) -> Result<IssueComment>;
    async fn list_issue_comments(&self, issue_id: Uuid) -> Result<Vec<IssueComment>>;

    async fn create_label(&self, repo_id: Uuid, name: &str, color: &str) -> Result<Label>;
    /// Rename and/or recolor a label. Conflicts if the new name is taken.
    async fn update_label(
        &self,
        repo_id: Uuid,
        label_id: Uuid,
        name: &str,
        color: &str,
    ) -> Result<Label>;
    /// Delete a label (it is removed from every issue). `false` if absent.
    async fn delete_label(&self, repo_id: Uuid, label_id: Uuid) -> Result<bool>;
    async fn add_issue_label(&self, issue_id: Uuid, label_id: Uuid) -> Result<()>;
    async fn list_issue_labels(&self, issue_id: Uuid) -> Result<Vec<Label>>;
    /// Replace all labels on an issue with the provided set (idempotent wipe+add).
    async fn replace_issue_labels(&self, issue_id: Uuid, label_ids: &[Uuid]) -> Result<()>;

    async fn add_issue_assignee(&self, issue_id: Uuid, user_id: Uuid) -> Result<()>;
    async fn list_issue_assignees(&self, issue_id: Uuid) -> Result<Vec<User>>;
    /// Replace all assignees on an issue with the provided set (idempotent wipe+add).
    async fn replace_issue_assignees(&self, issue_id: Uuid, user_ids: &[Uuid]) -> Result<()>;
}

/// Column list kept in sync with [`issue_from_row`].
const ISSUE_COLUMNS: &str =
    "id, repo_id, number, title, body, state, author_id, milestone_id, \
     created_at, updated_at, closed_at";

impl IssueStore for PgStore {
    async fn create_issue(
        &self,
        repo_id: Uuid,
        author_id: Option<Uuid>,
        input: &NewIssue,
    ) -> Result<Issue> {
        let mut tx = self.pool().begin().await.map_err(map_db)?;
        let number = next_repo_number(&mut tx, repo_id, "issues").await?;
        let row = sqlx::query(&format!(
            "INSERT INTO issues (repo_id, number, title, body, author_id) \
             VALUES ($1, $2, $3, $4, $5) \
             RETURNING {ISSUE_COLUMNS}"
        ))
        .bind(repo_id)
        .bind(number)
        .bind(&input.title)
        .bind(&input.body)
        .bind(author_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db)?;
        tx.commit().await.map_err(map_db)?;
        Ok(issue_from_row(&row))
    }

    async fn list_issues(&self, repo_id: Uuid, state: Option<IssueState>) -> Result<Vec<Issue>> {
        let base = format!(
            "SELECT {ISSUE_COLUMNS} FROM issues WHERE repo_id = $1"
        );
        let rows = match state {
            Some(state) => {
                sqlx::query(&format!("{base} AND state = $2 ORDER BY number DESC"))
                    .bind(repo_id)
                    .bind(state.as_str())
                    .fetch_all(self.pool())
                    .await
            }
            None => {
                sqlx::query(&format!("{base} ORDER BY number DESC"))
                    .bind(repo_id)
                    .fetch_all(self.pool())
                    .await
            }
        }
        .map_err(map_db)?;
        Ok(rows.iter().map(issue_from_row).collect())
    }

    async fn get_issue(&self, repo_id: Uuid, number: i32) -> Result<Issue> {
        let row = sqlx::query(&format!(
            "SELECT {ISSUE_COLUMNS} FROM issues WHERE repo_id = $1 AND number = $2"
        ))
        .bind(repo_id)
        .bind(number)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("issue", &number.to_string(), e))?;
        Ok(issue_from_row(&row))
    }

    async fn set_issue_state(
        &self,
        repo_id: Uuid,
        number: i32,
        state: IssueState,
    ) -> Result<Issue> {
        let row = sqlx::query(&format!(
            "UPDATE issues SET state = $3, updated_at = now(), \
                closed_at = CASE WHEN $3 = 'closed' THEN now() ELSE NULL END \
             WHERE repo_id = $1 AND number = $2 \
             RETURNING {ISSUE_COLUMNS}"
        ))
        .bind(repo_id)
        .bind(number)
        .bind(state.as_str())
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("issue", &number.to_string(), e))?;
        Ok(issue_from_row(&row))
    }

    async fn update_issue(
        &self,
        repo_id: Uuid,
        number: i32,
        input: &UpdateIssue,
    ) -> Result<Issue> {
        let row = sqlx::query(&format!(
            "UPDATE issues SET \
                title       = COALESCE($3, title), \
                body        = COALESCE($4, body), \
                milestone_id = CASE WHEN $5 THEN $6 ELSE milestone_id END, \
                updated_at  = now() \
             WHERE repo_id = $1 AND number = $2 \
             RETURNING {ISSUE_COLUMNS}"
        ))
        .bind(repo_id)
        .bind(number)
        .bind(&input.title)
        .bind(&input.body)
        .bind(input.set_milestone)
        .bind(input.milestone_id)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("issue", &number.to_string(), e))?;
        Ok(issue_from_row(&row))
    }

    async fn add_issue_comment(
        &self,
        issue_id: Uuid,
        author_id: Option<Uuid>,
        body: &str,
    ) -> Result<IssueComment> {
        let row = sqlx::query(
            "INSERT INTO issue_comments (issue_id, author_id, body) VALUES ($1, $2, $3) \
             RETURNING id, issue_id, author_id, body, created_at",
        )
        .bind(issue_id)
        .bind(author_id)
        .bind(body)
        .fetch_one(self.pool())
        .await
        .map_err(map_db)?;
        Ok(IssueComment {
            id: row.get("id"),
            issue_id: row.get("issue_id"),
            author_id: row.get("author_id"),
            body: row.get("body"),
            created_at: row.get("created_at"),
        })
    }

    async fn list_issue_comments(&self, issue_id: Uuid) -> Result<Vec<IssueComment>> {
        let rows = sqlx::query(
            "SELECT id, issue_id, author_id, body, created_at FROM issue_comments \
             WHERE issue_id = $1 ORDER BY created_at ASC",
        )
        .bind(issue_id)
        .fetch_all(self.pool())
        .await
        .map_err(map_db)?;
        Ok(rows
            .iter()
            .map(|row| IssueComment {
                id: row.get("id"),
                issue_id: row.get("issue_id"),
                author_id: row.get("author_id"),
                body: row.get("body"),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    async fn create_label(&self, repo_id: Uuid, name: &str, color: &str) -> Result<Label> {
        // Upsert: creating a label that already exists returns the existing one
        // (idempotent "ensure label"), so callers can attach by name freely.
        let row = sqlx::query(
            "INSERT INTO labels (repo_id, name, color) VALUES ($1, $2, $3) \
             ON CONFLICT (repo_id, name) DO UPDATE SET color = EXCLUDED.color \
             RETURNING id, repo_id, name, color",
        )
        .bind(repo_id)
        .bind(name)
        .bind(color)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("label", name, e))?;
        Ok(Label {
            id: row.get("id"),
            repo_id: row.get("repo_id"),
            name: row.get("name"),
            color: row.get("color"),
        })
    }

    async fn update_label(
        &self,
        repo_id: Uuid,
        label_id: Uuid,
        name: &str,
        color: &str,
    ) -> Result<Label> {
        let row = sqlx::query(
            "UPDATE labels SET name = $3, color = $4 WHERE repo_id = $1 AND id = $2 \
             RETURNING id, repo_id, name, color",
        )
        .bind(repo_id)
        .bind(label_id)
        .bind(name)
        .bind(color)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("label", name, e))?;
        Ok(Label {
            id: row.get("id"),
            repo_id: row.get("repo_id"),
            name: row.get("name"),
            color: row.get("color"),
        })
    }

    async fn delete_label(&self, repo_id: Uuid, label_id: Uuid) -> Result<bool> {
        let result = sqlx::query("DELETE FROM labels WHERE repo_id = $1 AND id = $2")
            .bind(repo_id)
            .bind(label_id)
            .execute(self.pool())
            .await
            .map_err(|e| CoreError::from_sqlx("label", &label_id.to_string(), e))?;
        Ok(result.rows_affected() > 0)
    }

    async fn add_issue_label(&self, issue_id: Uuid, label_id: Uuid) -> Result<()> {
        sqlx::query(
            "INSERT INTO issue_labels (issue_id, label_id) VALUES ($1, $2) \
             ON CONFLICT DO NOTHING",
        )
        .bind(issue_id)
        .bind(label_id)
        .execute(self.pool())
        .await
        .map_err(map_db)?;
        Ok(())
    }

    async fn list_issue_labels(&self, issue_id: Uuid) -> Result<Vec<Label>> {
        let rows = sqlx::query(
            "SELECT l.id, l.repo_id, l.name, l.color FROM labels l \
             JOIN issue_labels il ON il.label_id = l.id WHERE il.issue_id = $1 ORDER BY l.name",
        )
        .bind(issue_id)
        .fetch_all(self.pool())
        .await
        .map_err(map_db)?;
        Ok(rows
            .iter()
            .map(|row| Label {
                id: row.get("id"),
                repo_id: row.get("repo_id"),
                name: row.get("name"),
                color: row.get("color"),
            })
            .collect())
    }

    async fn add_issue_assignee(&self, issue_id: Uuid, user_id: Uuid) -> Result<()> {
        sqlx::query(
            "INSERT INTO issue_assignees (issue_id, user_id) VALUES ($1, $2) \
             ON CONFLICT DO NOTHING",
        )
        .bind(issue_id)
        .bind(user_id)
        .execute(self.pool())
        .await
        .map_err(map_db)?;
        Ok(())
    }

    async fn list_issue_assignees(&self, issue_id: Uuid) -> Result<Vec<User>> {
        let rows = sqlx::query(
            "SELECT u.id, u.username, u.email, u.created_at FROM users u \
             JOIN issue_assignees ia ON ia.user_id = u.id WHERE ia.issue_id = $1 ORDER BY u.username",
        )
        .bind(issue_id)
        .fetch_all(self.pool())
        .await
        .map_err(map_db)?;
        Ok(rows
            .iter()
            .map(|row| User {
                id: row.get("id"),
                username: row.get("username"),
                email: row.get("email"),
                created_at: row.get("created_at"),
            })
            .collect())
    }

    async fn replace_issue_assignees(&self, issue_id: Uuid, user_ids: &[Uuid]) -> Result<()> {
        let mut tx = self.pool().begin().await.map_err(map_db)?;
        sqlx::query("DELETE FROM issue_assignees WHERE issue_id = $1")
            .bind(issue_id)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        for &uid in user_ids {
            sqlx::query(
                "INSERT INTO issue_assignees (issue_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            )
            .bind(issue_id)
            .bind(uid)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        }
        tx.commit().await.map_err(map_db)?;
        Ok(())
    }

    async fn replace_issue_labels(&self, issue_id: Uuid, label_ids: &[Uuid]) -> Result<()> {
        let mut tx = self.pool().begin().await.map_err(map_db)?;
        sqlx::query("DELETE FROM issue_labels WHERE issue_id = $1")
            .bind(issue_id)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        for &lid in label_ids {
            sqlx::query(
                "INSERT INTO issue_labels (issue_id, label_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
            )
            .bind(issue_id)
            .bind(lid)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        }
        tx.commit().await.map_err(map_db)?;
        Ok(())
    }
}

fn map_db(e: sqlx::Error) -> CoreError {
    CoreError::from_sqlx("issue", "*", e)
}
