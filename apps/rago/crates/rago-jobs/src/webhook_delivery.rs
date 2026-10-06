//! `Webhooks::*` + `SendHttpService` runtime — build, sign, deliver (with
//! retry/backoff), and persist an outbound webhook delivery record.

use chrono::Utc;
use rago_db::Db;
use rago_integrations::webhooks::{deliver_signed, JwtSigner, Signing, Webhook, WebhookStatus};
use rago_integrations::HttpTransport;
use serde_json::Value;
use uuid::Uuid;

use crate::JobError;

/// Build, deliver, and persist a webhook. On a retryable failure it retries up
/// to `Webhook::RETRY_LIMIT` with the (deterministic) ActiveJob backoff, then
/// persists the final delivery record. Returns the persisted webhook.
#[allow(clippy::too_many_arguments)]
pub async fn emit_webhook<T: HttpTransport>(
    db: &Db,
    transport: &T,
    organization_id: Uuid,
    endpoint: &str,
    hmac_key: &str,
    webhook_type: &str,
    object_type: &str,
    object: Value,
) -> Result<Webhook, JobError> {
    let mut webhook = Webhook::build(organization_id, endpoint, webhook_type, object_type, object);

    // Deliver, retrying on transient failures. The backoff is short-circuited to
    // an immediate re-attempt here (the schedule is unit-tested separately); a
    // production worker re-enqueues after `backoff_seconds`.
    let algo = db.webhook_endpoint_algo(organization_id, endpoint).await?;
    let signing = Signing::for_algo(&algo, hmac_key, JwtSigner::global());
    loop {
        let should_retry = deliver_signed(transport, &mut webhook, signing).await;
        if !should_retry {
            break;
        }
    }

    db.upsert_webhook(
        webhook.id,
        webhook.organization_id,
        &webhook.webhook_type,
        &webhook.object_type,
        &webhook.endpoint,
        &webhook.payload,
        webhook.status.as_str(),
        webhook.http_status,
        webhook.retries,
    )
    .await?;

    // Append the terminal delivery attempt to the operator-facing feed. `retries`
    // counts re-attempts, so total attempts made is `retries + 1`. On success we
    // stamp `delivered_at`; otherwise the transport `response` is the last error.
    let delivered = webhook.status == WebhookStatus::Succeeded;
    let _ = db
        .record_webhook_delivery(rago_db::NewWebhookDelivery {
            organization_id: webhook.organization_id,
            endpoint_id: None,
            endpoint: &webhook.endpoint,
            event_type: &webhook.webhook_type,
            object_type: Some(&webhook.object_type),
            payload: &webhook.payload,
            status: webhook.status.as_str(),
            http_status: webhook.http_status,
            attempts: webhook.retries + 1,
            last_error: if delivered {
                None
            } else {
                webhook.response.as_deref()
            },
            delivered_at: delivered.then(|| Utc::now().naive_utc()),
        })
        .await;

    Ok(webhook)
}
