//! In-app notifications: fan a repo event out to its audience, plus per-user
//! listing and read-marking. The audience is the distinct set of the repo's org
//! members and explicit collaborators — minus the actor who caused the event.

use crate::error::{CoreError, Result};
use crate::store::PgStore;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

/// One notification.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Notification {
    pub id: Uuid,
    pub user_id: Uuid,
    pub repo_id: Option<Uuid>,
    pub kind: String,
    pub title: String,
    pub link: Option<String>,
    pub read: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

fn notification_from_row(row: &sqlx::postgres::PgRow) -> Notification {
    Notification {
        id: row.get("id"),
        user_id: row.get("user_id"),
        repo_id: row.get("repo_id"),
        kind: row.get("kind"),
        title: row.get("title"),
        link: row.get("link"),
        read: row.get("read"),
        created_at: row.get("created_at"),
    }
}

/// Notification persistence + fan-out.
#[allow(async_fn_in_trait)]
pub trait NotifyStore {
    /// Create one notification for a specific user.
    async fn notify_user(
        &self,
        user_id: Uuid,
        repo_id: Option<Uuid>,
        kind: &str,
        title: &str,
        link: Option<&str>,
    ) -> Result<()>;

    /// Notify the repo's audience (org members ∪ collaborators) except `actor`.
    /// Returns how many notifications were created.
    async fn notify_repo_audience(
        &self,
        repo_id: Uuid,
        org_id: Uuid,
        actor: Option<Uuid>,
        kind: &str,
        title: &str,
        link: Option<&str>,
    ) -> Result<u64>;

    async fn list_for_user(&self, user_id: Uuid, limit: i64) -> Result<Vec<Notification>>;
    async fn unread_count(&self, user_id: Uuid) -> Result<i64>;
    async fn mark_all_read(&self, user_id: Uuid) -> Result<()>;
}

impl NotifyStore for PgStore {
    async fn notify_user(
        &self,
        user_id: Uuid,
        repo_id: Option<Uuid>,
        kind: &str,
        title: &str,
        link: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO notifications (user_id, repo_id, kind, title, link) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(user_id)
        .bind(repo_id)
        .bind(kind)
        .bind(title)
        .bind(link)
        .execute(self.pool())
        .await
        .map_err(map_db)?;
        Ok(())
    }

    async fn notify_repo_audience(
        &self,
        repo_id: Uuid,
        org_id: Uuid,
        actor: Option<Uuid>,
        kind: &str,
        title: &str,
        link: Option<&str>,
    ) -> Result<u64> {
        // Insert one row per audience member in a single statement. The audience
        // is org members ∪ repo collaborators, minus the actor.
        let result = sqlx::query(
            "INSERT INTO notifications (user_id, repo_id, kind, title, link) \
             SELECT uid, $1, $4, $5, $6 FROM ( \
                 SELECT user_id AS uid FROM memberships WHERE org_id = $2 \
                 UNION \
                 SELECT user_id AS uid FROM repo_collaborators WHERE repo_id = $1 \
             ) audience \
             WHERE $3::uuid IS NULL OR uid <> $3::uuid",
        )
        .bind(repo_id)
        .bind(org_id)
        .bind(actor)
        .bind(kind)
        .bind(title)
        .bind(link)
        .execute(self.pool())
        .await
        .map_err(map_db)?;
        Ok(result.rows_affected())
    }

    async fn list_for_user(&self, user_id: Uuid, limit: i64) -> Result<Vec<Notification>> {
        let rows = sqlx::query(
            "SELECT id, user_id, repo_id, kind, title, link, read, created_at \
             FROM notifications WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(user_id)
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .map_err(map_db)?;
        Ok(rows.iter().map(notification_from_row).collect())
    }

    async fn unread_count(&self, user_id: Uuid) -> Result<i64> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND NOT read")
                .bind(user_id)
                .fetch_one(self.pool())
                .await
                .map_err(map_db)?;
        Ok(count)
    }

    async fn mark_all_read(&self, user_id: Uuid) -> Result<()> {
        sqlx::query("UPDATE notifications SET read = true WHERE user_id = $1")
            .bind(user_id)
            .execute(self.pool())
            .await
            .map_err(map_db)?;
        Ok(())
    }
}

fn map_db(e: sqlx::Error) -> CoreError {
    CoreError::from_sqlx("notification", "*", e)
}
