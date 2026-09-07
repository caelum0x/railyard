//! Outbound webhook subscriptions + delivery records.
//!
//! Delivery itself (signing + HTTP + retry) lives in the server, which owns the
//! async runtime; this module owns only the persistence: which URL is subscribed
//! to which events for a repo, and an audit trail of delivery attempts.

use crate::error::{CoreError, Result};
use crate::store::PgStore;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

/// A webhook subscription.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Webhook {
    pub id: Uuid,
    pub repo_id: Uuid,
    pub url: String,
    /// Not serialized back out — the secret is write-only.
    #[serde(skip_serializing)]
    pub secret: String,
    pub events: Vec<String>,
    pub active: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

/// Input to create a webhook.
#[derive(Clone, Debug, Deserialize)]
pub struct NewWebhook {
    pub url: String,
    pub secret: String,
    #[serde(default)]
    pub events: Vec<String>,
}

fn webhook_from_row(row: &sqlx::postgres::PgRow) -> Webhook {
    Webhook {
        id: row.get("id"),
        repo_id: row.get("repo_id"),
        url: row.get("url"),
        secret: row.get("secret"),
        events: row.get("events"),
        active: row.get("active"),
        created_at: row.get("created_at"),
    }
}

/// Webhook persistence.
#[allow(async_fn_in_trait)]
pub trait WebhookStore {
    async fn create_webhook(&self, repo_id: Uuid, input: &NewWebhook) -> Result<Webhook>;
    async fn list_webhooks(&self, repo_id: Uuid) -> Result<Vec<Webhook>>;
    /// Active webhooks for a repo that subscribe to `event`.
    async fn webhooks_for_event(&self, repo_id: Uuid, event: &str) -> Result<Vec<Webhook>>;
    /// Record the outcome of a delivery attempt sequence.
    async fn record_delivery(
        &self,
        webhook_id: Uuid,
        event: &str,
        success: bool,
        status_code: Option<i32>,
        attempts: i32,
        error: Option<&str>,
    ) -> Result<()>;
}

impl WebhookStore for PgStore {
    async fn create_webhook(&self, repo_id: Uuid, input: &NewWebhook) -> Result<Webhook> {
        let row = sqlx::query(
            "INSERT INTO webhooks (repo_id, url, secret, events) VALUES ($1, $2, $3, $4) \
             RETURNING id, repo_id, url, secret, events, active, created_at",
        )
        .bind(repo_id)
        .bind(&input.url)
        .bind(&input.secret)
        .bind(&input.events)
        .fetch_one(self.pool())
        .await
        .map_err(map_db)?;
        Ok(webhook_from_row(&row))
    }

    async fn list_webhooks(&self, repo_id: Uuid) -> Result<Vec<Webhook>> {
        let rows = sqlx::query(
            "SELECT id, repo_id, url, secret, events, active, created_at FROM webhooks \
             WHERE repo_id = $1 ORDER BY created_at DESC",
        )
        .bind(repo_id)
        .fetch_all(self.pool())
        .await
        .map_err(map_db)?;
        Ok(rows.iter().map(webhook_from_row).collect())
    }

    async fn webhooks_for_event(&self, repo_id: Uuid, event: &str) -> Result<Vec<Webhook>> {
        let rows = sqlx::query(
            "SELECT id, repo_id, url, secret, events, active, created_at FROM webhooks \
             WHERE repo_id = $1 AND active AND $2 = ANY(events)",
        )
        .bind(repo_id)
        .bind(event)
        .fetch_all(self.pool())
        .await
        .map_err(map_db)?;
        Ok(rows.iter().map(webhook_from_row).collect())
    }

    async fn record_delivery(
        &self,
        webhook_id: Uuid,
        event: &str,
        success: bool,
        status_code: Option<i32>,
        attempts: i32,
        error: Option<&str>,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO webhook_deliveries (webhook_id, event, success, status_code, attempts, error) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(webhook_id)
        .bind(event)
        .bind(success)
        .bind(status_code)
        .bind(attempts)
        .bind(error)
        .execute(self.pool())
        .await
        .map_err(map_db)?;
        Ok(())
    }
}

fn map_db(e: sqlx::Error) -> CoreError {
    CoreError::from_sqlx("webhook", "*", e)
}
