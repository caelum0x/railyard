//! Durable job outbox: domain types, constants, and the persistence trait.
//!
//! Every forge side-effect (webhook delivery, notification fan-out, audit
//! append, email) is enqueued here as a row in `outbox_jobs`. The worker
//! loop in `rithub-server` claims batches and executes them with backoff.
//! Because the enqueue write hits the same Postgres connection pool as the
//! main mutation, the two are durable together even under crash.

use crate::error::{CoreError, Result};
use crate::store::PgStore;
use serde_json::Value;
use sqlx::Row;
use time::OffsetDateTime;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Job kind constants
// ---------------------------------------------------------------------------

/// Deliver a webhook event to every active subscription for a repo.
///
/// Payload: `{ "repo_id": uuid, "event": str, "event_payload": object }`
pub const KIND_WEBHOOK_DELIVERY: &str = "webhook_delivery";

/// Fan a forge event out to the repo audience as in-app notifications.
///
/// Payload: `{ "repo_id": uuid, "org_id": uuid, "actor_id": uuid|null,
///             "kind": str, "title": str, "link": str|null }`
pub const KIND_NOTIFICATION_FAN_OUT: &str = "notification_fan_out";

/// Append a single row to the immutable audit log.
///
/// Payload: `{ "repo_id": uuid|null, "actor_id": uuid|null,
///             "actor_name": str, "action": str,
///             "target": str|null, "detail": str|null }`
pub const KIND_AUDIT_APPEND: &str = "audit_append";

/// Dispatch CI for a pushed/updated commit: seed a `queued` check run and fan a
/// `check_run` webhook out to any listening external runners.
///
/// Payload: `{ "repo_id": uuid, "org": str, "repo": str, "head_sha": str,
///             "ref": str, "event": str }`
pub const KIND_CHECK_RUN_DISPATCH: &str = "check_run_dispatch";

/// Send a transactional email to every current audience member of a repo
/// (org members ∪ collaborators, minus the actor).
///
/// Payload: `{ "repo_id": uuid, "org_id": uuid, "actor_id": uuid|null,
///             "subject": str, "body_text": str }`
pub const KIND_EMAIL_NOTIFICATION: &str = "email_notification";

// ---------------------------------------------------------------------------
// Status constants (TEXT in Postgres, never enum — consistent with the rest
// of the codebase which uses plain TEXT + Rust enums/consts for domain vals).
// ---------------------------------------------------------------------------

pub const STATUS_PENDING: &str = "pending";
pub const STATUS_RUNNING: &str = "running";
pub const STATUS_SUCCEEDED: &str = "succeeded";
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_DEAD: &str = "dead";

/// Maximum execution attempts before a job is permanently moved to `dead`.
pub const MAX_ATTEMPTS: i32 = 5;

// ---------------------------------------------------------------------------
// Domain struct
// ---------------------------------------------------------------------------

/// One row from `outbox_jobs`.
#[derive(Clone, Debug)]
pub struct OutboxJob {
    pub id: Uuid,
    pub kind: String,
    pub payload: Value,
    pub status: String,
    pub attempts: i32,
    pub run_after: OffsetDateTime,
    pub created_at: OffsetDateTime,
    pub last_error: Option<String>,
}

fn job_from_row(row: &sqlx::postgres::PgRow) -> OutboxJob {
    OutboxJob {
        id: row.get("id"),
        kind: row.get("kind"),
        payload: row.get("payload"),
        status: row.get("status"),
        attempts: row.get("attempts"),
        run_after: row.get("run_after"),
        created_at: row.get("created_at"),
        last_error: row.get("last_error"),
    }
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Persistence operations for the job outbox.
#[allow(async_fn_in_trait)]
pub trait OutboxStore {
    /// Enqueue a new job. Returns its generated id.
    async fn enqueue_job(&self, kind: &str, payload: Value) -> Result<Uuid>;

    /// Claim up to `limit` runnable jobs (status `pending`|`failed`,
    /// `run_after` <= now()), atomically marking them `running`.
    ///
    /// Uses `SELECT … FOR UPDATE SKIP LOCKED` so concurrent workers never
    /// double-claim a job.
    async fn claim_jobs(&self, limit: i32) -> Result<Vec<OutboxJob>>;

    /// Mark a job `succeeded`; it will not be retried.
    async fn ack_success(&self, job_id: Uuid) -> Result<()>;

    /// Mark a job `failed`, record the error, and schedule a retry with
    /// exponential backoff. Moves to `dead` when attempts reaches
    /// [`MAX_ATTEMPTS`].
    async fn ack_failure(
        &self,
        job_id: Uuid,
        error: &str,
        next_run_after: OffsetDateTime,
    ) -> Result<()>;

    /// Return the distinct email addresses of a repo's audience (org members
    /// ∪ repo collaborators), excluding `exclude_actor` if provided.
    /// Used by the email job handler to enumerate recipients.
    async fn audience_emails(
        &self,
        repo_id: Uuid,
        org_id: Uuid,
        exclude_actor: Option<Uuid>,
    ) -> Result<Vec<String>>;

    /// List jobs filtered by optional status, newest first, capped at `limit`.
    async fn list_jobs(&self, status: Option<&str>, limit: i64) -> Result<Vec<OutboxJob>>;

    /// Reset a dead or failed job to `pending` so the worker retries it.
    async fn redeliver_job(&self, job_id: Uuid) -> Result<OutboxJob>;
}

// ---------------------------------------------------------------------------
// PgStore impl
// ---------------------------------------------------------------------------

impl OutboxStore for PgStore {
    async fn enqueue_job(&self, kind: &str, payload: Value) -> Result<Uuid> {
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO outbox_jobs (kind, payload) VALUES ($1, $2) RETURNING id",
        )
        .bind(kind)
        .bind(sqlx::types::Json(&payload))
        .fetch_one(self.pool())
        .await
        .map_err(map_db)?;
        Ok(id)
    }

    async fn claim_jobs(&self, limit: i32) -> Result<Vec<OutboxJob>> {
        // CTE selects candidate ids with SKIP LOCKED, UPDATE marks them running
        // in the same statement — atomic claim with no extra round trip.
        let rows = sqlx::query(
            "WITH batch AS (
                SELECT id
                FROM outbox_jobs
                WHERE status IN ('pending', 'failed')
                  AND run_after <= now()
                ORDER BY run_after, id
                LIMIT $1
                FOR UPDATE SKIP LOCKED
            )
            UPDATE outbox_jobs j
            SET status = 'running'
            FROM batch
            WHERE j.id = batch.id
            RETURNING j.id, j.kind, j.payload, j.status, j.attempts,
                      j.run_after, j.created_at, j.last_error",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await
        .map_err(map_db)?;
        Ok(rows.iter().map(job_from_row).collect())
    }

    async fn ack_success(&self, job_id: Uuid) -> Result<()> {
        sqlx::query(
            "UPDATE outbox_jobs SET status = 'succeeded' WHERE id = $1",
        )
        .bind(job_id)
        .execute(self.pool())
        .await
        .map_err(map_db)?;
        Ok(())
    }

    async fn ack_failure(
        &self,
        job_id: Uuid,
        error: &str,
        next_run_after: OffsetDateTime,
    ) -> Result<()> {
        sqlx::query(
            "UPDATE outbox_jobs
             SET status     = CASE WHEN attempts + 1 >= $3 THEN 'dead' ELSE 'failed' END,
                 attempts   = attempts + 1,
                 run_after  = $4,
                 last_error = $2
             WHERE id = $1",
        )
        .bind(job_id)
        .bind(error)
        .bind(MAX_ATTEMPTS)
        .bind(next_run_after)
        .execute(self.pool())
        .await
        .map_err(map_db)?;
        Ok(())
    }

    async fn audience_emails(
        &self,
        repo_id: Uuid,
        org_id: Uuid,
        exclude_actor: Option<Uuid>,
    ) -> Result<Vec<String>> {
        let rows: Vec<String> = sqlx::query_scalar(
            "SELECT DISTINCT u.email
             FROM users u
             WHERE u.id IN (
                 SELECT user_id FROM memberships WHERE org_id = $2
                 UNION
                 SELECT user_id FROM repo_collaborators WHERE repo_id = $1
             )
             AND ($3::uuid IS NULL OR u.id <> $3::uuid)",
        )
        .bind(repo_id)
        .bind(org_id)
        .bind(exclude_actor)
        .fetch_all(self.pool())
        .await
        .map_err(map_db)?;
        Ok(rows)
    }

    async fn list_jobs(&self, status: Option<&str>, limit: i64) -> Result<Vec<OutboxJob>> {
        let rows = match status {
            Some(s) => {
                sqlx::query(
                    "SELECT id, kind, payload, status, attempts, run_after, created_at, last_error
                     FROM outbox_jobs
                     WHERE status = $1
                     ORDER BY created_at DESC
                     LIMIT $2",
                )
                .bind(s)
                .bind(limit)
                .fetch_all(self.pool())
                .await
                .map_err(map_db)?
            }
            None => {
                sqlx::query(
                    "SELECT id, kind, payload, status, attempts, run_after, created_at, last_error
                     FROM outbox_jobs
                     ORDER BY created_at DESC
                     LIMIT $1",
                )
                .bind(limit)
                .fetch_all(self.pool())
                .await
                .map_err(map_db)?
            }
        };
        Ok(rows.iter().map(job_from_row).collect())
    }

    async fn redeliver_job(&self, job_id: Uuid) -> Result<OutboxJob> {
        let row = sqlx::query(
            "UPDATE outbox_jobs
             SET status = 'pending', run_after = now(), attempts = 0, last_error = NULL
             WHERE id = $1
               AND status IN ('dead', 'failed')
             RETURNING id, kind, payload, status, attempts, run_after, created_at, last_error",
        )
        .bind(job_id)
        .fetch_one(self.pool())
        .await
        .map_err(|e| CoreError::from_sqlx("outbox_job", &job_id.to_string(), e))?;
        Ok(job_from_row(&row))
    }
}

fn map_db(e: sqlx::Error) -> CoreError {
    CoreError::from_sqlx("outbox_job", "*", e)
}
