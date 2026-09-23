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
//!
//! # Full trigger map (75-entry catalog)
//!
//! The table below is the single source of truth for where each of the 75
//! Lago-parity webhook types fires in Rago today. "[LIVE]" = wired and emitting.
//!
//! | webhook_type | trigger crate / file |
//! |---|---|
//! | alert.triggered **[LIVE]** | rago-api phase14/phase15 (`evaluate_usage_alerts`, `evaluate_lifetime_alerts`) |
//! | billable_metric.created **[LIVE]** | rago-graphql mutation.rs |
//! | billable_metric.updated **[LIVE]** | rago-graphql admin_mutations.rs |
//! | billable_metric.deleted **[LIVE]** | rago-graphql admin_mutations.rs |
//! | dunning_campaign.finished **[LIVE]** | rago-jobs dunning.rs (`process_dunning_campaigns`) |
//! | invoice.created **[LIVE]** | rago-jobs clock/invoices.rs (`finalize_invoices`) + scheduler.rs |
//! | invoice.one_off_created | no-op until one-off invoice create path |
//! | invoice.paid_credit_added | no-op until `Credits::AppliedPrepaidCreditService` |
//! | invoice.generated | no-op until PDF generation path |
//! | invoice.drafted **[LIVE]** | rago-jobs clock/invoices.rs (`refresh_draft_invoices`) |
//! | invoice.ready_to_finalize | no-op until tax pull / finalize path |
//! | invoice.voided | no-op until void service |
//! | invoice.deleted | no-op until invoice delete service |
//! | invoice.payment_dispute_lost | no-op until dispute service |
//! | invoice.payment_status_updated **[LIVE]** | rago-jobs scheduler.rs (billing charge) + rago-api webhooks.rs (Stripe webhook) |
//! | invoice.payment_overdue **[LIVE]** | rago-jobs clock/invoices.rs (`mark_invoices_as_payment_overdue`) |
//! | invoice.payment_failure | no-op until payment failure delivery path |
//! | invoice.resynced | no-op until accounting sync path |
//! | event.error | no-op until event post-process path |
//! | events.errors | no-op until event validation path |
//! | fee.created **[LIVE]** | rago-jobs scheduler.rs (billing cycle per-fee) |
//! | fee.tax_provider_error | no-op until tax provider fee error path |
//! | customer.created **[LIVE]** | rago-api handlers.rs (`upsert_customer`) |
//! | customer.updated **[LIVE]** | rago-api handlers.rs (`upsert_customer`) |
//! | customer.accounting_provider_created | no-op until accounting sync |
//! | customer.accounting_provider_error | no-op until accounting sync error |
//! | customer.crm_provider_created | no-op until CRM sync |
//! | customer.crm_provider_error | no-op until CRM sync error |
//! | customer.payment_provider_created | no-op until provider-customer create |
//! | customer.payment_provider_error | no-op until provider-customer error |
//! | customer.checkout_url_generated | no-op until checkout URL service |
//! | customer.tax_provider_error | no-op until tax provider customer error |
//! | customer.vies_check | no-op until EU auto-taxes VIES path |
//! | credit_note.created **[LIVE]** | rago-api handlers.rs + rago-graphql admin_mutations.rs |
//! | credit_note.generated | no-op until PDF generation path |
//! | credit_note.refund_failure | no-op until refund failure delivery path |
//! | integration.provider_error | no-op until integration provider error path |
//! | order.created | no-op until orders schema (schema-deferred) |
//! | order.executed **[LIVE]** | rago-graphql admin_mutations5.rs (`execute_order`) |
//! | order_form.created | no-op until order_forms schema (schema-deferred) |
//! | order_form.signed **[LIVE]** | rago-graphql admin_mutations5.rs (`sign_order_form`) |
//! | order_form.expired | no-op until order_forms schema (schema-deferred) |
//! | order_form.voided **[LIVE]** | rago-graphql admin_mutations5.rs (`void_order_form`) |
//! | payment.requires_action | no-op until 3DS / SCA payment flow |
//! | payment.succeeded | no-op until provider payment success path |
//! | payment_provider.error | no-op until provider error delivery path |
//! | payment_receipt.created | no-op until payment_receipts schema (schema-deferred) |
//! | payment_receipt.generated | no-op until payment_receipts schema (schema-deferred) |
//! | payment_request.created **[LIVE]** | rago-jobs dunning.rs (`create_and_charge_payment_request`) |
//! | payment_request.payment_failure | no-op until payment failure delivery path |
//! | payment_request.payment_status_updated | no-op until payment request update service |
//! | plan.created **[LIVE]** | rago-graphql mutation.rs |
//! | plan.deleted **[LIVE]** | rago-graphql admin_mutations.rs |
//! | plan.updated **[LIVE]** | rago-graphql admin_mutations.rs |
//! | feature.created **[LIVE]** | rago-api resources.rs |
//! | feature.updated **[LIVE]** | rago-api resources.rs |
//! | feature.deleted **[LIVE]** | rago-api resources.rs |
//! | quote.created **[LIVE]** | rago-graphql admin_mutations5.rs |
//! | quote.approved **[LIVE]** | rago-graphql admin_mutations5.rs |
//! | quote.voided **[LIVE]** | rago-graphql admin_mutations5.rs |
//! | subscription.canceled | no-op until cancel service |
//! | subscription.incomplete | no-op until incomplete-activation path |
//! | subscription.terminated **[LIVE]** | rago-jobs clock/subscriptions.rs (`terminate_ended_subscriptions`) |
//! | subscription.started **[LIVE]** | rago-jobs clock/subscriptions.rs (`activate_subscriptions`) |
//! | subscription.termination_alert **[LIVE]** | rago-jobs clock/subscriptions.rs (`subscriptions_to_be_terminated`) |
//! | subscription.trial_ended **[LIVE]** | rago-jobs clock/subscriptions.rs (`free_trial_biller`) |
//! | subscription.updated **[LIVE]** | rago-graphql admin_mutations.rs |
//! | subscription.usage_threshold_reached | no-op until `UsageMonitoring::CheckThresholdsService` |
//! | wallet.created | no-op until wallet create service |
//! | wallet.updated | no-op until wallet update service |
//! | wallet.terminated **[LIVE]** | rago-jobs clock/wallets.rs (`terminate_wallets`) |
//! | wallet.depleted_ongoing_balance **[LIVE]** | rago-jobs clock/wallets.rs (`refresh_wallets_ongoing_balance`) |
//! | wallet_transaction.created **[LIVE]** | rago-jobs clock/wallets.rs (`create_interval_wallet_transactions`) |
//! | wallet_transaction.updated | no-op until settle / mark-as-failed service |
//! | wallet_transaction.payment_failure | no-op until wallet payment failure delivery path |

use rago_db::Db;
use rago_integrations::webhooks::WebhookKind;
use serde_json::Value;
use uuid::Uuid;

use crate::JobError;

/// The webhook types emitted by [`rago-jobs`] clock / scheduler / dunning jobs
/// from a real domain trigger today. This is the jobs-layer slice of the full
/// [`WebhookKind`] catalog; see the module docstring for the complete 75-entry
/// trigger map across all crates.
///
/// Events fired by other crates but NOT listed here:
/// * rago-api: `alert.triggered`, `customer.created`/`updated`,
///   `credit_note.created`, `feature.*`, `invoice.payment_status_updated`
///   (Stripe incoming webhook handler);
/// * rago-graphql: `billable_metric.*`, `plan.*`, `subscription.updated`,
///   `quote.*`, `order_form.signed`/`voided`, `order.executed`,
///   `credit_note.created`;
/// * schema-deferred / no trigger yet: everything else — see module docstring.
pub const JOB_EMITTED_WEBHOOKS: &[WebhookKind] = &[
    // clock/invoices.rs
    WebhookKind::InvoiceCreated,           // finalize_invoices + scheduler billing cycle
    WebhookKind::InvoiceDrafted,           // refresh_draft_invoices
    WebhookKind::InvoicePaymentOverdue,    // mark_invoices_as_payment_overdue
    // scheduler.rs (billing cycle)
    WebhookKind::FeeCreated,                     // per assembled fee in billing cycle
    WebhookKind::InvoicePaymentStatusUpdated,     // post-charge status update
    // clock/subscriptions.rs
    WebhookKind::SubscriptionStarted,
    WebhookKind::SubscriptionTerminated,
    WebhookKind::SubscriptionTerminationAlert,
    WebhookKind::SubscriptionTrialEnded,
    // clock/wallets.rs
    WebhookKind::WalletTerminated,
    WebhookKind::WalletDepletedOngoingBalance,
    WebhookKind::WalletTransactionCreated,
    // dunning.rs
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
    let Some((endpoint_id, endpoint)) = db.first_webhook_endpoint_id(organization_id).await? else {
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

    // Append a row to the operator-facing deliveries feed. The work item was just
    // materialized `pending` (attempt 0, not yet delivered); the delivery worker
    // records the terminal attempt separately (see `webhook_delivery`).
    let _ = db
        .record_webhook_delivery(rago_db::NewWebhookDelivery {
            organization_id,
            endpoint_id: Some(endpoint_id),
            endpoint: &webhook.endpoint,
            event_type: &webhook.webhook_type,
            object_type: Some(&webhook.object_type),
            payload: &webhook.payload,
            status: "pending",
            http_status: None,
            attempts: 0,
            last_error: None,
            delivered_at: None,
        })
        .await;
    Ok(true)
}
