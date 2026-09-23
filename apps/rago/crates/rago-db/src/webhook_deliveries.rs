//! Repository extensions for the append-only webhook deliveries feed
//! (`webhook_deliveries`).
//!
//! Unlike the `webhooks` work-item table (one mutable row per queued/retrying
//! delivery), this is the delivery *history* an operator reads: a row is
//! recorded at each dispatch point in the webhook path
//! ([`crate::Db::record_webhook_delivery`]) and never mutated afterward.
//!
//! All reads are org-scoped and parameterized; optional filters use the
//! `($n IS NULL OR col = $n)` form so a single static query serves every
//! filter combination.

use chrono::{DateTime, NaiveDateTime, Utc};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

use crate::error::Result;
use crate::repo::Db;

/// One recorded outbound webhook delivery (a row in the deliveries feed).
#[derive(Debug, Clone)]
pub struct WebhookDeliveryRow {
    pub id: Uuid,
    pub organization_id: Uuid,
    pub endpoint_id: Option<Uuid>,
    pub endpoint: String,
    pub event_type: String,
    pub object_type: Option<String>,
    pub payload: Value,
    pub status: String,
    pub http_status: Option<i32>,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
}

/// Parameters for recording one delivery row (grouped to avoid a many-argument
/// insert signature).
#[derive(Debug, Clone)]
pub struct NewWebhookDelivery<'a> {
    pub organization_id: Uuid,
    pub endpoint_id: Option<Uuid>,
    pub endpoint: &'a str,
    pub event_type: &'a str,
    pub object_type: Option<&'a str>,
    pub payload: &'a Value,
    pub status: &'a str,
    pub http_status: Option<i32>,
    pub attempts: i32,
    pub last_error: Option<&'a str>,
    pub delivered_at: Option<NaiveDateTime>,
}

fn delivery_row(r: &sqlx::postgres::PgRow) -> WebhookDeliveryRow {
    WebhookDeliveryRow {
        id: r.get("id"),
        organization_id: r.get("organization_id"),
        endpoint_id: r.get("endpoint_id"),
        endpoint: r.get("endpoint"),
        event_type: r.get("event_type"),
        object_type: r.get("object_type"),
        payload: r.get("payload"),
        status: r.get("status"),
        http_status: r.get("http_status"),
        attempts: r.get("attempts"),
        last_error: r.get("last_error"),
        created_at: r.get::<NaiveDateTime, _>("created_at").and_utc(),
        delivered_at: r
            .get::<Option<NaiveDateTime>, _>("delivered_at")
            .map(|t| t.and_utc()),
    }
}

impl Db {
    /// Append one delivery row to the feed, returning its id. Best-effort by
    /// convention: callers on the emit path already committed their state
    /// change, so a failure here must never fail the trigger.
    pub async fn record_webhook_delivery(&self, d: NewWebhookDelivery<'_>) -> Result<Uuid> {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO webhook_deliveries \
                 (id, organization_id, endpoint_id, endpoint, event_type, object_type, \
                  payload, status, http_status, attempts, last_error, created_at, delivered_at) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11, now(), $12)",
        )
        .bind(id)
        .bind(d.organization_id)
        .bind(d.endpoint_id)
        .bind(d.endpoint)
        .bind(d.event_type)
        .bind(d.object_type)
        .bind(d.payload)
        .bind(d.status)
        .bind(d.http_status)
        .bind(d.attempts)
        .bind(d.last_error)
        .bind(d.delivered_at)
        .execute(&self.pool)
        .await?;
        Ok(id)
    }

    /// The org's first configured webhook endpoint `(id, url)` — the target the
    /// pending-materialization path records against. `None` when the org has no
    /// endpoint configured.
    pub async fn first_webhook_endpoint_id(&self, org: Uuid) -> Result<Option<(Uuid, String)>> {
        let row = sqlx::query(
            "SELECT id, webhook_url FROM webhook_endpoints \
             WHERE organization_id = $1 ORDER BY created_at ASC LIMIT 1",
        )
        .bind(org)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| (r.get::<Uuid, _>("id"), r.get::<String, _>("webhook_url"))))
    }

    /// A page of the deliveries feed for an org, newest first, with optional
    /// `endpoint_id` / `event_type` / `status` filters.
    pub async fn list_webhook_deliveries(
        &self,
        org: Uuid,
        endpoint_id: Option<Uuid>,
        event_type: Option<&str>,
        status: Option<&str>,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<WebhookDeliveryRow>> {
        let rows = sqlx::query(
            "SELECT id, organization_id, endpoint_id, endpoint, event_type, object_type, \
                    payload, status, http_status, attempts, last_error, created_at, delivered_at \
             FROM webhook_deliveries \
             WHERE organization_id = $1 \
               AND ($2::uuid IS NULL OR endpoint_id = $2) \
               AND ($3::varchar IS NULL OR event_type = $3) \
               AND ($4::varchar IS NULL OR status = $4) \
             ORDER BY created_at DESC LIMIT $5 OFFSET $6",
        )
        .bind(org)
        .bind(endpoint_id)
        .bind(event_type)
        .bind(status)
        .bind(limit)
        .bind(offset)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(delivery_row).collect())
    }

    /// Total deliveries matching the same filters (for pagination metadata).
    pub async fn count_webhook_deliveries(
        &self,
        org: Uuid,
        endpoint_id: Option<Uuid>,
        event_type: Option<&str>,
        status: Option<&str>,
    ) -> Result<i64> {
        let row = sqlx::query(
            "SELECT COUNT(*)::bigint AS n FROM webhook_deliveries \
             WHERE organization_id = $1 \
               AND ($2::uuid IS NULL OR endpoint_id = $2) \
               AND ($3::varchar IS NULL OR event_type = $3) \
               AND ($4::varchar IS NULL OR status = $4)",
        )
        .bind(org)
        .bind(endpoint_id)
        .bind(event_type)
        .bind(status)
        .fetch_one(&self.pool)
        .await?;
        Ok(row.get::<i64, _>("n"))
    }

    /// A single delivery, org-scoped (a cross-tenant id resolves to `None`).
    pub async fn find_webhook_delivery(
        &self,
        org: Uuid,
        id: Uuid,
    ) -> Result<Option<WebhookDeliveryRow>> {
        let row = sqlx::query(
            "SELECT id, organization_id, endpoint_id, endpoint, event_type, object_type, \
                    payload, status, http_status, attempts, last_error, created_at, delivered_at \
             FROM webhook_deliveries WHERE id = $1 AND organization_id = $2",
        )
        .bind(id)
        .bind(org)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.as_ref().map(delivery_row))
    }
}
