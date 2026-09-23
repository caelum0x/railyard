//! Trigger-point webhook emission for clock/background jobs.
//!
//! Ports the `SendWebhookJob.perform_later(<type>, <object>)` call that Lago's
//! services fire at each lifecycle transition. Unlike [`crate::webhook_delivery`]
//! (which builds, signs, and immediately delivers over an
//! [`rago_integrations::HttpTransport`]), the clock jobs have no transport — they
//! run against `&Db` only. So, exactly like Lago enqueuing `SendWebhookJob`, they
//! persist a **pending** webhook row that the delivery worker
//! ([`crate::queue::redeliver_due_webhooks`] / `SendHttpWebhookJob`) later picks
//! up and delivers.
//!
//! Every emit routes through [`WebhookKind`] so the persisted row carries the
//! exact `webhook_type`/`object_type` pair and the correctly-enveloped payload
//! from the catalog (the single source of truth), never a hand-typed string.

use rago_db::Db;
use rago_integrations::webhooks::WebhookKind;
use serde_json::Value;
use uuid::Uuid;

use crate::JobError;

/// The webhook types the clock/background jobs in this crate actually emit from
/// a real domain trigger today — the `record_webhook` call sites in
/// [`crate::clock`] and [`crate::dunning`]. This is the jobs-layer slice of the
/// full [`WebhookKind`] catalog, the single place to read trigger coverage.
///
/// Catalog kinds absent here are **no-op-until-trigger** from the jobs layer —
/// defined for parity but wired at their real trigger in the owning crate:
///
/// * fired elsewhere: `invoice.payment_status_updated` (rago-api payment
///   webhook), `customer.created`/`customer.updated`, `plan.*`,
///   `billable_metric.*`, `feature.*`, `quote.*`, `wallet.created`/`updated`,
///   `wallet_transaction.updated`, `subscription.started`/`updated` etc. from
///   rago-billing/rago-graphql mutations, and `payment.requires_action`,
///   `payment.succeeded`, `*_provider_error`, `*.payment_failure`,
///   `credit_note.*`, `*.resynced` from the rago-integrations payment/provider
///   path;
/// * schema-deferred: orders, order_forms, payment_receipts — see
///   [`WebhookKind::is_schema_deferred`].
pub const JOB_EMITTED_WEBHOOKS: &[WebhookKind] = &[
    WebhookKind::InvoiceCreated,
    WebhookKind::InvoiceDrafted,
    WebhookKind::InvoicePaymentOverdue,
    WebhookKind::SubscriptionStarted,
    WebhookKind::SubscriptionTerminated,
    WebhookKind::SubscriptionTerminationAlert,
    WebhookKind::SubscriptionTrialEnded,
    WebhookKind::WalletTerminated,
    WebhookKind::WalletDepletedOngoingBalance,
    WebhookKind::WalletTransactionCreated,
    WebhookKind::DunningCampaignFinished,
    WebhookKind::PaymentRequestCreated,
];

/// Record a pending outbound webhook for a job trigger point.
///
/// Faithful to Lago: a `SendWebhookJob` materializes one `Webhook` per
/// configured `webhook_endpoint`. With no endpoint configured the org receives
/// nothing, so this is a **no-op** (returns `Ok(false)`) rather than writing an
/// undeliverable row. When an endpoint exists, the row is stored `pending` with
/// the full signed-ready envelope built by [`WebhookKind::build`], and the
/// delivery worker completes it.
///
/// Returns `true` when a webhook row was written, `false` on the no-endpoint
/// no-op.
pub async fn record_webhook(
    db: &Db,
    organization_id: Uuid,
    kind: WebhookKind,
    object: Value,
) -> Result<bool, JobError> {
    let Some(endpoint) = db.webhook_endpoint(organization_id).await? else {
        return Ok(false);
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
    .await?;
    Ok(true)
}
