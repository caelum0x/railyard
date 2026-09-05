//! Trigger-point webhook emission for GraphQL admin mutations.
//!
//! Mirrors `rago_jobs::webhook_emit::record_webhook`: at each Lago mutation
//! trigger (`Plans::CreateService`, `BillableMetrics::UpdateService`,
//! `Orders::ExecuteService`, …) Lago enqueues a `SendWebhookJob` that
//! materializes one `Webhook` per configured `webhook_endpoint`. Here we do the
//! same — build the signed-ready envelope from the [`WebhookKind`] catalog (the
//! single source of truth for the `webhook_type`/`object_type` pair) and persist
//! it `pending` for the delivery worker to pick up.
//!
//! Endpoint-gated like Lago: with no endpoint configured the org receives
//! nothing, so this is a no-op. Emission is best-effort — the mutation's state
//! change has already committed, so a webhook persistence hiccup is logged into
//! the returned unit and never propagated as a mutation error (matching Lago's
//! fire-and-forget `perform_later`).

use rago_db::Db;
use rago_integrations::webhooks::WebhookKind;
use serde_json::Value;
use uuid::Uuid;

/// Record a pending outbound webhook for a mutation trigger point. Returns
/// `true` when a row was written, `false` on the no-endpoint no-op or on a
/// swallowed persistence error.
pub async fn emit(db: &Db, organization_id: Uuid, kind: WebhookKind, object: Value) -> bool {
    let endpoint = match db.webhook_endpoint(organization_id).await {
        Ok(Some(e)) => e,
        Ok(None) => return false,
        Err(_) => return false,
    };
    let webhook = kind.build(organization_id, endpoint, object);
    db.upsert_webhook(
        webhook.id,
        webhook.organization_id,
        &webhook.webhook_type,
        &webhook.object_type,
        &webhook.endpoint,
        &webhook.payload,
        "pending",
        None,
        0,
    )
    .await
    .is_ok()
}
