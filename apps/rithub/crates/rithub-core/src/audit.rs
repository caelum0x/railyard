//! Append-only audit log for security-relevant events. Records are only ever
//! inserted (never updated or deleted through this API), giving an immutable
//! accountability trail: pushes, rejected pushes, membership/role changes, PR
//! merges, protection changes, and token creation.

use crate::error::{CoreError, Result};
use crate::store::PgStore;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

/// One audit event.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditEvent {
    pub id: Uuid,
    pub repo_id: Option<Uuid>,
    pub actor_id: Option<Uuid>,
    pub actor_name: String,
    pub action: String,
    pub target: Option<String>,
    pub detail: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// What to record.
pub struct NewAuditEvent<'a> {
    pub repo_id: Option<Uuid>,
    pub actor_id: Option<Uuid>,
    pub actor_name: &'a str,
    pub action: &'a str,
    pub target: Option<&'a str>,
    pub detail: Option<&'a str>,
}

fn event_from_row(row: &sqlx::postgres::PgRow) -> AuditEvent {
    AuditEvent {
        id: row.get("id"),
        repo_id: row.get("repo_id"),
        actor_id: row.get("actor_id"),
        actor_name: row.get("actor_name"),
        action: row.get("action"),
        target: row.get("target"),
        detail: row.get("detail"),
        created_at: row.get("created_at"),
    }
}

/// Audit persistence (append + query).
#[allow(async_fn_in_trait)]
pub trait AuditStore {
    async fn record_event(&self, event: NewAuditEvent<'_>) -> Result<()>;
    async fn list_repo_events(&self, repo_id: Uuid, limit: i64) -> Result<Vec<AuditEvent>>;
}

impl AuditStore for PgStore {
    async fn record_event(&self, event: NewAuditEvent<'_>) -> Result<()> {
        sqlx::query(
            "INSERT INTO audit_events (repo_id, actor_id, actor_name, action, target, detail) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(event.repo_id)
        .bind(event.actor_id)
        .bind(event.actor_name)
        .bind(event.action)
        .bind(event.target)
        .bind(event.detail)
        .execute(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("audit", event.action, e))?;
        Ok(())
    }

    async fn list_repo_events(&self, repo_id: Uuid, limit: i64) -> Result<Vec<AuditEvent>> {
        let rows = sqlx::query(
            "SELECT id, repo_id, actor_id, actor_name, action, target, detail, created_at \
             FROM audit_events WHERE repo_id = $1 ORDER BY created_at DESC LIMIT $2",
        )
        .bind(repo_id)
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("audit", "*", e))?;
        Ok(rows.iter().map(event_from_row).collect())
    }
}
