//! `Webhooks::*` + `SendHttpService` runtime — build, sign, deliver (with
//! retry/backoff), and persist an outbound webhook delivery record.

use rago_db::Db;
use rago_integrations::webhooks::{deliver, Webhook};
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
    loop {
        let should_retry = deliver(transport, &mut webhook, hmac_key).await;
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

    Ok(webhook)
}
