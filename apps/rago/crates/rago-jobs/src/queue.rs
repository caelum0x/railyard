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

use crate::scheduler::bill_and_collect_external;
use crate::JobError;

/// Job type: bill a subscription (invoice + webhooks + payment collection).
pub const JOB_BILL_SUBSCRIPTION: &str = "bill_subscription";
/// Job type: generate a dashboard-requested data export.
pub const JOB_DATA_EXPORT_GENERATE: &str = "data_exports.generate";
/// Job type: collect (charge) one invoice — the operator "retry payment" action.
pub const JOB_COLLECT_INVOICE: &str = "invoices.collect";

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
            // Full pipeline: invoice → invoice.created/fee.created webhooks →
            // collect payment through the org's provider.
            let transport = rago_integrations::default_transport();
            let outcome = bill_and_collect_external(db, &transport, org, ext, date).await?;
            tracing::info!(
                invoice_id = %outcome.invoice_id,
                created = outcome.created,
                collection = ?outcome.collection,
                "bill_subscription job done"
            );
            Ok(())
        }
        JOB_DATA_EXPORT_GENERATE => {
            let export_id = uuid_field(payload, "data_export_id")?;
            let (org, resource_type) = db
                .data_export_owner(export_id)
                .await?
                .ok_or_else(|| JobError::Integration("data_export_not_found".into()))?;
            if let Err(e) = crate::data_export::process(db, export_id, org, &resource_type).await {
                db.fail_data_export(export_id).await?;
                return Err(e);
            }
            Ok(())
        }
        crate::integrations_sync::JOB_SYNC_INVOICE => {
            let invoice_id = uuid_field(payload, "invoice_id")?;
            let org = invoice_org(db, invoice_id).await?;
            let transport = rago_integrations::default_transport();
            crate::integrations_sync::sync_invoice_accounting(db, &transport, org, invoice_id).await?;
            Ok(())
        }
        crate::integrations_sync::JOB_SYNC_CREDIT_NOTE => {
            let cn_id = uuid_field(payload, "credit_note_id")?;
            let org = db
                .find_credit_note(cn_id)
                .await?
                .map(|c| c.organization_id)
                .ok_or_else(|| JobError::Integration("credit_note_not_found".into()))?;
            let transport = rago_integrations::default_transport();
            crate::integrations_sync::sync_credit_note_accounting(db, &transport, org, cn_id).await?;
            Ok(())
        }
        crate::payment_sync::JOB_SYNC_PAYMENT => {
            let payment_id = uuid_field(payload, "payment_id")?;
            let org = db
                .syncable_payment(payment_id)
                .await?
                .map(|p| p.organization_id)
                .ok_or_else(|| JobError::Integration("payment_not_found".into()))?;
            let transport = rago_integrations::default_transport();
            crate::payment_sync::sync_payment_accounting(db, &transport, org, payment_id).await?;
            Ok(())
        }
        crate::crm_sync::JOB_SYNC_CRM_CUSTOMER => {
            let customer_id = uuid_field(payload, "customer_id")?;
            let customer = db
                .find_customer(customer_id)
                .await?
                .ok_or_else(|| JobError::Integration("customer_not_found".into()))?;
            let transport = rago_integrations::default_transport();
            crate::crm_sync::sync_customer_crm(db, &transport, customer.organization_id, &customer).await?;
            Ok(())
        }
        crate::crm_sync::JOB_SYNC_CRM_INVOICE => {
            let invoice_id = uuid_field(payload, "invoice_id")?;
            let org = invoice_org(db, invoice_id).await?;
            let transport = rago_integrations::default_transport();
            crate::crm_sync::sync_invoice_crm(db, &transport, org, invoice_id).await?;
            Ok(())
        }
        crate::crm_sync::JOB_SYNC_CRM_SUBSCRIPTION => {
            let subscription_id = uuid_field(payload, "subscription_id")?;
            let org = db
                .subscription_org(subscription_id)
                .await?
                .ok_or_else(|| JobError::Integration("subscription_not_found".into()))?;
            let transport = rago_integrations::default_transport();
            crate::crm_sync::sync_subscription_crm(db, &transport, org, subscription_id).await?;
            Ok(())
        }
        crate::integrations_sync::JOB_TAX_VOIDING => {
            let invoice_id = uuid_field(payload, "invoice_id")?;
            let org = invoice_org(db, invoice_id).await?;
            let transport = rago_integrations::default_transport();
            crate::integrations_sync::void_invoice_taxes(db, &transport, org, invoice_id).await?;
            Ok(())
        }
        crate::integrations_sync::JOB_CREDIT_NOTE_TAX_REPORTING => {
            let cn_id = uuid_field(payload, "credit_note_id")?;
            let org = db
                .find_credit_note(cn_id)
                .await?
                .map(|c| c.organization_id)
                .ok_or_else(|| JobError::Integration("credit_note_not_found".into()))?;
            let transport = rago_integrations::default_transport();
            crate::integrations_sync::report_credit_note_taxes(db, &transport, org, cn_id).await?;
            Ok(())
        }
        crate::pay_in_advance::JOB_PAY_IN_ADVANCE => {
            let event_id = uuid_field(payload, "event_id")?;
            crate::pay_in_advance::bill_event_in_advance(db, event_id).await?;
            Ok(())
        }
        JOB_COLLECT_INVOICE => {
            let invoice_id = uuid_field(payload, "invoice_id")?;
            let invoice = db
                .find_invoice(invoice_id)
                .await?
                .ok_or_else(|| JobError::Integration("invoice_not_found".into()))?;
            let transport = rago_integrations::default_transport();
            crate::collection::collect_invoice(db, &transport, &invoice).await?;
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
        crate::vies::JOB_VIES_CHECK => {
            let customer_id = uuid_field(payload, "customer_id")?;
            let transport = rago_integrations::default_transport();
            crate::vies::check_customer_vies(db, &transport, customer_id).await?;
            Ok(())
        }
        other => Err(JobError::Payment(format!("unknown job type: {other}"))),
    }
}

fn uuid_field(payload: &Value, key: &str) -> Result<Uuid, JobError> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(|| JobError::Integration(format!("bad {key}")))
}

async fn invoice_org(db: &Db, invoice_id: Uuid) -> Result<Uuid, JobError> {
    db.find_invoice(invoice_id)
        .await?
        .map(|i| i.organization_id)
        .ok_or_else(|| JobError::Integration("invoice_not_found".into()))
}

/// Deliver due webhook work items — freshly queued `pending` rows and
/// `retrying` rows whose backoff elapsed — over the injected transport, signed
/// with the organization's own HMAC key. Succeeded → `succeeded`; still failing
/// → `retrying` with backoff, or `failed` once the retry limit is reached. The
/// linked delivery-feed row is updated with each outcome.
pub async fn redeliver_due_webhooks<T: HttpTransport>(
    db: &Db,
    transport: &T,
) -> Result<usize, JobError> {
    let due = db.due_retrying_webhooks(Utc::now().naive_utc()).await?;
    let mut delivered = 0;
    for w in due {
        let hmac_key = db.ensure_organization_hmac_key(w.organization_id).await?;

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
        let succeeded = webhook.status == rago_integrations::webhooks::WebhookStatus::Succeeded;
        let _ = db
            .update_webhook_delivery_for_webhook(
                webhook.id,
                webhook.status.as_str(),
                webhook.http_status,
                webhook.retries + i32::from(succeeded),
                if succeeded { None } else { webhook.response.as_deref() },
                succeeded,
            )
            .await;
        if succeeded {
            delivered += 1;
        }
    }
    Ok(delivered)
}
