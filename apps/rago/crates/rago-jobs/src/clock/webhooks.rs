//! Webhook housekeeping clocks.
//!
//! Ports `Clock::{WebhooksCleanup, InboundWebhooksCleanup, InboundWebhooksRetry}Job`.

use chrono::{Duration, Utc};
use rago_db::Db;

use crate::JobError;

/// Retention window for delivery records (Lago default 90 days).
const RETENTION_DAYS: i64 = 90;
/// Max reprocessing attempts before an inbound webhook is no longer retried.
const MAX_INBOUND_ATTEMPTS: i32 = 5;

/// `Clock::WebhooksCleanupJob` — delete outbound delivery records older than the
/// retention window. Returns the number deleted.
pub async fn webhooks_cleanup(db: &Db) -> Result<u64, JobError> {
    let cutoff = (Utc::now() - Duration::days(RETENTION_DAYS)).naive_utc();
    let r = sqlx::query("DELETE FROM webhooks WHERE updated_at < $1")
        .bind(cutoff)
        .execute(&db.pool)
        .await
        .map_err(rago_db::DbError::from)?;
    Ok(r.rows_affected())
}

/// `Clock::InboundWebhooksCleanupJob` — delete inbound webhooks older than the
/// retention window. Returns the number deleted.
pub async fn inbound_webhooks_cleanup(db: &Db) -> Result<u64, JobError> {
    let cutoff = (Utc::now() - Duration::days(RETENTION_DAYS)).naive_utc();
    let r = sqlx::query("DELETE FROM inbound_webhooks WHERE updated_at < $1")
        .bind(cutoff)
        .execute(&db.pool)
        .await
        .map_err(rago_db::DbError::from)?;
    Ok(r.rows_affected())
}

/// `Clock::InboundWebhooksRetryJob` → `InboundWebhooks::ProcessJob`. Moves
/// retriable inbound webhooks (pending/failed, under the attempt cap) into
/// `processing` and bumps their attempt counter. Returns the number re-queued.
pub async fn inbound_webhooks_retry(db: &Db) -> Result<u64, JobError> {
    let r = sqlx::query(
        "UPDATE inbound_webhooks SET status = 'processing', \
             processing_attempts = processing_attempts + 1, updated_at = now() \
         WHERE status IN ('pending', 'failed') AND processing_attempts < $1",
    )
    .bind(MAX_INBOUND_ATTEMPTS)
    .execute(&db.pool)
    .await
    .map_err(rago_db::DbError::from)?;
    Ok(r.rows_affected())
}
