//! Trigger-point webhook emission for REST handlers.
//!
//! Thin best-effort wrapper over [`rago_jobs::webhook_emit::record_webhook`] —
//! the same endpoint-gated "materialize one pending `Webhook` per configured
//! endpoint" path the clock jobs use. At each Lago mutation trigger
//! (`Customers::UpsertFromApiService`, `CreditNotes::CreateService`,
//! `Entitlement::Features::*Service`, …) we record the signed-ready envelope
//! built from the [`WebhookKind`] catalog for the delivery worker to pick up.
//!
//! Emission is best-effort: the handler's state change has already committed, so
//! a no-endpoint no-op or a persistence hiccup must never turn a successful
//! mutation into an error (matching Lago's fire-and-forget `perform_later`).

use rago_db::Db;
use rago_integrations::webhooks::WebhookKind;
use serde_json::Value;
use uuid::Uuid;

/// Record a pending outbound webhook for a REST trigger point. Errors and the
/// no-endpoint case are swallowed (the resource is already persisted).
pub(crate) async fn webhook(db: &Db, organization_id: Uuid, kind: WebhookKind, object: Value) {
    let _ = rago_jobs::webhook_emit::record_webhook(db, organization_id, kind, object).await;
}
