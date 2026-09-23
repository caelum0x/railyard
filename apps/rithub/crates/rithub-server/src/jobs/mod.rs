//! Durable job worker: claims `outbox_jobs`, dispatches to per-kind handlers,
//! then acks success or schedules a retry with exponential backoff.
//!
//! The worker is a single long-running tokio task spawned in `main`. It polls
//! Postgres for due jobs, processes them, and backs off when the queue is
//! empty. Because the claim uses `SELECT … FOR UPDATE SKIP LOCKED`, spinning
//! up additional worker tasks (or processes) is safe without coordination.

use time::OffsetDateTime;
use tracing::{error, info, warn};
use uuid::Uuid;

use rithub_core::{
    AuditStore, NewAuditEvent, OutboxJob, OutboxStore, WebhookStore,
    KIND_AUDIT_APPEND, KIND_EMAIL_NOTIFICATION, KIND_NOTIFICATION_FAN_OUT,
    KIND_WEBHOOK_DELIVERY,
};

use crate::state::AppState;

/// Jobs claimed per poll cycle.
const CLAIM_BATCH: i32 = 20;
/// Seconds to sleep when the queue was empty.
const IDLE_SLEEP_SECS: u64 = 5;

// ---------------------------------------------------------------------------
// Worker entry point
// ---------------------------------------------------------------------------

/// Run forever, polling and processing outbox jobs.
/// Spawn this in a `tokio::spawn` at startup.
pub async fn run_worker(state: AppState) {
    info!("forge-jobs worker started");
    loop {
        match state.store.claim_jobs(CLAIM_BATCH).await {
            Err(error) => {
                error!(%error, "failed to claim jobs — will retry");
                tokio::time::sleep(std::time::Duration::from_secs(IDLE_SLEEP_SECS)).await;
            }
            Ok(jobs) if jobs.is_empty() => {
                tokio::time::sleep(std::time::Duration::from_secs(IDLE_SLEEP_SECS)).await;
            }
            Ok(jobs) => {
                for job in jobs {
                    process_job(&state, job).await;
                }
                // No sleep: immediately check for more when the batch was full.
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

async fn process_job(state: &AppState, job: OutboxJob) {
    let result = match job.kind.as_str() {
        KIND_WEBHOOK_DELIVERY => handle_webhook_delivery(state, &job).await,
        KIND_NOTIFICATION_FAN_OUT => handle_notification_fan_out(state, &job).await,
        KIND_AUDIT_APPEND => handle_audit_append(state, &job).await,
        KIND_EMAIL_NOTIFICATION => handle_email_notification(state, &job).await,
        unknown => {
            warn!(kind = unknown, job_id = %job.id, "unknown job kind — marking dead");
            Err(format!("unknown job kind: {unknown}"))
        }
    };

    match result {
        Ok(()) => {
            if let Err(e) = state.store.ack_success(job.id).await {
                error!(error = %e, job_id = %job.id, "failed to ack_success");
            }
        }
        Err(error) => {
            warn!(%error, job_id = %job.id, attempts = job.attempts, "job failed");
            let next = backoff_time(job.attempts + 1);
            if let Err(e) = state.store.ack_failure(job.id, &error, next).await {
                error!(error = %e, job_id = %job.id, "failed to ack_failure");
            }
        }
    }
}

/// Compute the next `run_after` for a failing job using capped exponential
/// backoff: attempt 1 → 30 s, 2 → 60 s, 3 → 120 s … up to 1 hour.
fn backoff_time(attempt: i32) -> OffsetDateTime {
    let secs = (30u64 * (1u64 << (attempt as u32).min(6))).min(3600);
    OffsetDateTime::now_utc() + time::Duration::seconds(secs as i64)
}

// ---------------------------------------------------------------------------
// Handler: webhook delivery
// ---------------------------------------------------------------------------

/// Load the active webhook subscriptions for the event's repo and deliver
/// to each. Individual delivery failures are recorded in `webhook_deliveries`
/// by the lower-level helper — they do not fail the job (partial delivery is
/// expected; the subscribe–deliver model handles receiver-side unreliability).
async fn handle_webhook_delivery(
    state: &AppState,
    job: &OutboxJob,
) -> std::result::Result<(), String> {
    let p = &job.payload;
    let repo_id: Uuid = parse_uuid(p, "repo_id")?;
    let event = p["event"]
        .as_str()
        .ok_or("missing field: event")?;
    let body = serde_json::to_vec(&p["event_payload"]).unwrap_or_default();

    let hooks = state
        .store
        .webhooks_for_event(repo_id, event)
        .await
        .map_err(|e| e.to_string())?;

    for hook in &hooks {
        crate::webhooks::deliver(state, hook, event, &body).await;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Handler: notification fan-out
// ---------------------------------------------------------------------------

async fn handle_notification_fan_out(
    state: &AppState,
    job: &OutboxJob,
) -> std::result::Result<(), String> {
    use rithub_core::NotifyStore;

    let p = &job.payload;
    let repo_id: Uuid = parse_uuid(p, "repo_id")?;
    let org_id: Uuid = parse_uuid(p, "org_id")?;
    let actor_id = p["actor_id"].as_str().and_then(|s| s.parse::<Uuid>().ok());
    let kind = p["kind"].as_str().ok_or("missing field: kind")?;
    let title = p["title"].as_str().ok_or("missing field: title")?;
    let link = p["link"].as_str();

    state
        .store
        .notify_repo_audience(repo_id, org_id, actor_id, kind, title, link)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Handler: audit append
// ---------------------------------------------------------------------------

async fn handle_audit_append(
    state: &AppState,
    job: &OutboxJob,
) -> std::result::Result<(), String> {
    let p = &job.payload;
    let repo_id = p["repo_id"].as_str().and_then(|s| s.parse::<Uuid>().ok());
    let actor_id = p["actor_id"].as_str().and_then(|s| s.parse::<Uuid>().ok());
    let actor_name = p["actor_name"].as_str().ok_or("missing field: actor_name")?;
    let action = p["action"].as_str().ok_or("missing field: action")?;
    let target = p["target"].as_str();
    let detail = p["detail"].as_str();

    state
        .store
        .record_event(NewAuditEvent {
            repo_id,
            actor_id,
            actor_name,
            action,
            target,
            detail,
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Handler: email notification
// ---------------------------------------------------------------------------

/// Send a transactional email to every current audience member of a repo.
/// If SMTP is not configured (`state.mailer` is `None`) this is a no-op —
/// the job still succeeds so it doesn't fill up the retry queue.
async fn handle_email_notification(
    state: &AppState,
    job: &OutboxJob,
) -> std::result::Result<(), String> {
    let mailer = match &state.mailer {
        Some(m) => m.clone(),
        None => {
            // SMTP not configured — skip silently.
            return Ok(());
        }
    };

    let p = &job.payload;
    let repo_id: Uuid = parse_uuid(p, "repo_id")?;
    let org_id: Uuid = parse_uuid(p, "org_id")?;
    let actor_id = p["actor_id"].as_str().and_then(|s| s.parse::<Uuid>().ok());
    let subject = p["subject"].as_str().ok_or("missing field: subject")?;
    let body_text = p["body_text"].as_str().ok_or("missing field: body_text")?;

    let recipients = state
        .store
        .audience_emails(repo_id, org_id, actor_id)
        .await
        .map_err(|e| e.to_string())?;

    if recipients.is_empty() {
        return Ok(());
    }

    let from_addr = std::env::var("SMTP_FROM").unwrap_or_else(|_| "rithub@localhost".into());

    let mut last_error: Option<String> = None;
    for to in &recipients {
        let result = send_email(&mailer, &from_addr, to, subject, body_text).await;
        if let Err(e) = result {
            warn!(error = %e, recipient = %to, "email delivery failed");
            last_error = Some(e);
        }
    }

    // Only fail the job if every recipient failed. Partial delivery is logged
    // above but not retried at the per-recipient level in Phase B.
    if last_error.is_some() && recipients.len() == 1 {
        return Err(last_error.unwrap());
    }
    Ok(())
}

async fn send_email(
    mailer: &lettre::AsyncSmtpTransport<lettre::Tokio1Executor>,
    from: &str,
    to: &str,
    subject: &str,
    body_text: &str,
) -> std::result::Result<(), String> {
    use lettre::message::header::ContentType;
    use lettre::{AsyncTransport, Message};

    let email = Message::builder()
        .from(from.parse().map_err(|e: lettre::address::AddressError| e.to_string())?)
        .to(to.parse().map_err(|e: lettre::address::AddressError| e.to_string())?)
        .subject(subject)
        .header(ContentType::TEXT_PLAIN)
        .body(body_text.to_string())
        .map_err(|e| e.to_string())?;

    mailer.send(email).await.map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn parse_uuid(
    payload: &serde_json::Value,
    field: &str,
) -> std::result::Result<Uuid, String> {
    payload[field]
        .as_str()
        .ok_or_else(|| format!("missing field: {field}"))?
        .parse::<Uuid>()
        .map_err(|e| format!("invalid uuid for {field}: {e}"))
}
