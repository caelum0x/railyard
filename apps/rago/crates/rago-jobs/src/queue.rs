//! Durable job queue — the worker loop, cron sweeps, and webhook re-delivery
//! that replace the in-memory scheduling.
//!
//! Backed by the Postgres `jobs` table (`rago_db` enqueue/lease/ack/fail).
//! Jobs are leased with `FOR UPDATE SKIP LOCKED`, run by [`dispatch`], then
//! acked or rescheduled with the ActiveJob backoff (`attempts^4 + 2` seconds);
//! after `max_attempts` they are dead-lettered.

use chrono::{NaiveDate, Utc};
use rago_db::Db;
use rago_integrations::webhooks::{deliver, Webhook};
use rago_integrations::HttpTransport;
use serde_json::{json, Value};
use std::str::FromStr;
use uuid::Uuid;

use crate::scheduler::bill_subscription_external;
use crate::JobError;

/// Job type: bill a subscription.
pub const JOB_BILL_SUBSCRIPTION: &str = "bill_subscription";

/// ActiveJob-style backoff for the `attempts`-th failure (deterministic).
pub fn backoff_secs(attempts: i32) -> i64 {
    (attempts as i64).pow(4) + 2
}

/// Enqueue billing jobs for every subscription due on `billing_date` (the
/// scheduler's enqueue side — the worker runs them).
pub async fn enqueue_billing_cycle(
    db: &Db,
    billing_date: NaiveDate,
    force: bool,
) -> Result<Vec<Uuid>, JobError> {
    use rago_billing::dates::due_for_billing;
    let mut ids = Vec::new();
    for sub in db.active_subscriptions().await? {
        let plan = db.find_plan(sub.plan_id).await?;
        let interval = crate::scheduler::interval_of(&plan.interval);
        let anniversary = matches!(sub.billing_time, rago_core::enums::BillingTime::Anniversary);
        let sub_at = sub.subscription_at.map(|t| t.date_naive()).unwrap_or(billing_date);
        if !force && !due_for_billing(interval, anniversary, sub_at, billing_date) {
            continue;
        }
        let payload = json!({
            "organization_id": sub.organization_id.to_string(),
            "subscription_external_id": sub.external_id,
            "billing_date": billing_date.format("%Y-%m-%d").to_string(),
        });
        let id = db
            .enqueue_job(JOB_BILL_SUBSCRIPTION, &payload, Utc::now().naive_utc(), 5)
            .await?;
        ids.push(id);
    }
    Ok(ids)
}

/// Lease and run one due job, returning `Some(job_type)` if one ran.
pub async fn run_one(db: &Db, worker: &str) -> Result<Option<String>, JobError> {
    let Some(job) = db.lease_job(worker).await? else {
        return Ok(None);
    };
    let job_type = job.job_type.clone();
    match dispatch(db, &job.job_type, &job.payload, job.attempts).await {
        Ok(()) => {
            db.complete_job(job.id).await?;
        }
        Err(e) => {
            // Retry with backoff, or dead-letter when attempts are exhausted.
            let will_retry = (job.attempts + 1) < job.max_attempts;
            let retry_in = will_retry.then(|| backoff_secs(job.attempts));
            db.fail_job(job.id, &e.to_string(), retry_in).await?;
        }
    }
    Ok(Some(job_type))
}

/// Run leased jobs until the queue is empty (one pass).
pub async fn drain(db: &Db, worker: &str) -> Result<usize, JobError> {
    let mut n = 0;
    while run_one(db, worker).await?.is_some() {
        n += 1;
    }
    Ok(n)
}

/// Dispatch a job by type. `attempts` is the number of prior failures (used by
/// the `simulate_failure_until_attempt` test hook to prove durable retries).
async fn dispatch(db: &Db, job_type: &str, payload: &Value, attempts: i32) -> Result<(), JobError> {
    match job_type {
        JOB_BILL_SUBSCRIPTION => {
            if let Some(until) = payload.get("simulate_failure_until_attempt").and_then(Value::as_i64) {
                if (attempts as i64) < until {
                    return Err(JobError::Payment("simulated transient failure".into()));
                }
            }
            let org = payload
                .get("organization_id")
                .and_then(Value::as_str)
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| JobError::Payment("bad organization_id".into()))?;
            let ext = payload
                .get("subscription_external_id")
                .and_then(Value::as_str)
                .ok_or_else(|| JobError::Payment("bad subscription_external_id".into()))?;
            let date = payload
                .get("billing_date")
                .and_then(Value::as_str)
                .and_then(|s| NaiveDate::from_str(s).ok())
                .ok_or_else(|| JobError::Payment("bad billing_date".into()))?;
            bill_subscription_external(db, org, ext, date).await?;
            Ok(())
        }
        crate::data_export::JOB_DATA_EXPORT => {
            let export_id = payload
                .get("data_export_id")
                .and_then(Value::as_str)
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| JobError::Payment("bad data_export_id".into()))?;
            let org = payload
                .get("organization_id")
                .and_then(Value::as_str)
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| JobError::Payment("bad organization_id".into()))?;
            let resource_type = payload
                .get("resource_type")
                .and_then(Value::as_str)
                .ok_or_else(|| JobError::Payment("bad resource_type".into()))?;
            crate::data_export::process(db, export_id, org, resource_type).await?;
            Ok(())
        }
        other => Err(JobError::Payment(format!("unknown job type: {other}"))),
    }
}

/// Re-deliver webhooks in `retrying` state that are due, using the injected
/// transport. Succeeded → `succeeded`; still failing → `retrying` with backoff,
/// or `failed` once the retry limit is reached.
pub async fn redeliver_due_webhooks<T: HttpTransport>(
    db: &Db,
    transport: &T,
) -> Result<usize, JobError> {
    let due = db.due_retrying_webhooks(Utc::now().naive_utc()).await?;
    let mut delivered = 0;
    for w in due {
        let hmac_key = db
            .organization_hmac_key(w.organization_id)
            .await?
            .unwrap_or_else(|| "rago-default-key".into());

        // Rebuild a Webhook carrying the persisted state so `deliver` updates it.
        let mut webhook = Webhook {
            id: w.id,
            organization_id: w.organization_id,
            webhook_type: w.webhook_type.clone(),
            object_type: w.object_type.clone(),
            endpoint: w.endpoint.clone(),
            payload: w.payload.clone(),
            status: rago_integrations::webhooks::WebhookStatus::Retrying,
            http_status: None,
            response: None,
            retries: w.retries,
            last_retried_at: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let retrying = deliver(transport, &mut webhook, &hmac_key).await;
        let next_retry = retrying.then(|| backoff_secs(webhook.retries));
        db.update_webhook_retry(
            webhook.id,
            webhook.status.as_str(),
            webhook.http_status,
            webhook.retries,
            next_retry,
        )
        .await?;
        if webhook.status == rago_integrations::webhooks::WebhookStatus::Succeeded {
            delivered += 1;
        }
    }
    Ok(delivered)
}
