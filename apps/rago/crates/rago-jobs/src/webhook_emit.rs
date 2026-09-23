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
//! | invoice.one_off_created **[LIVE]** | rago-api breadth.rs (`create_one_off_invoice`) |
//! | invoice.paid_credit_added | no-op until `Credits::AppliedPrepaidCreditService` |
//! | invoice.generated **[LIVE]** | rago-jobs clock/invoices.rs + scheduler.rs, rago-api subs.rs (`finalize_invoice`) + breadth.rs (one-off) — `file_url` → `/invoices/:id/download` |
//! | invoice.drafted **[LIVE]** | rago-jobs clock/invoices.rs (`refresh_draft_invoices`) |
//! | invoice.ready_to_finalize | no-op until tax pull / finalize path |
//! | invoice.voided **[LIVE]** | rago-api subs.rs (`void_invoice`) |
//! | invoice.deleted | no-op until invoice delete service |
//! | invoice.payment_dispute_lost **[LIVE]** | rago-api subs.rs (`lose_dispute`) |
//! | invoice.payment_status_updated **[LIVE]** | rago-jobs collection.rs (every charge) + rago-api webhooks.rs (provider webhooks) |
//! | invoice.payment_overdue **[LIVE]** | rago-jobs clock/invoices.rs (`mark_invoices_as_payment_overdue`) |
//! | invoice.payment_failure **[LIVE]** | rago-jobs collection.rs (decline / no payment method) + rago-api webhooks.rs |
//! | invoice.resynced **[LIVE]** | rago-jobs integrations_sync.rs (`sync_invoice_accounting`) |
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
//! | customer.payment_provider_created **[LIVE]** | rago-jobs collection.rs (Stripe customer created) |
//! | customer.payment_provider_error | no-op until provider-customer error |
//! | customer.checkout_url_generated **[LIVE]** | rago-jobs collection.rs (`checkout_url`) |
//! | customer.tax_provider_error | no-op until tax provider customer error |
//! | customer.vies_check | no-op until EU auto-taxes VIES path |
//! | credit_note.created **[LIVE]** | rago-api handlers.rs + rago-graphql admin_mutations.rs |
//! | credit_note.generated **[LIVE]** | rago-api handlers.rs + rago-graphql admin_mutations.rs (after creation) — `file_url` → `/credit_notes/:id/download` |
//! | credit_note.refund_failure **[LIVE]** | rago-jobs refunds.rs (provider refund declined) |
//! | integration.provider_error | no-op until integration provider error path |
//! | order.created | no-op until orders schema (schema-deferred) |
//! | order.executed **[LIVE]** | rago-graphql admin_mutations5.rs (`execute_order`) |
//! | order_form.created | no-op until order_forms schema (schema-deferred) |
//! | order_form.signed **[LIVE]** | rago-graphql admin_mutations5.rs (`sign_order_form`) |
//! | order_form.expired | no-op until order_forms schema (schema-deferred) |
//! | order_form.voided **[LIVE]** | rago-graphql admin_mutations5.rs (`void_order_form`) |
//! | payment.requires_action | no-op until 3DS / SCA payment flow |
//! | payment.succeeded **[LIVE]** | rago-jobs collection.rs + rago-api webhooks.rs |
//! | payment_provider.error **[LIVE]** | rago-api webhooks.rs (provider decline event) |
//! | payment_receipt.created | no-op until payment_receipts schema (schema-deferred) |
//! | payment_receipt.generated | no-op until payment_receipts schema (schema-deferred) |
//! | payment_request.created **[LIVE]** | rago-jobs dunning.rs (`create_and_charge_payment_request`) |
//! | payment_request.payment_failure **[LIVE]** | rago-jobs dunning.rs (an invoice of the request could not be paid) |
//! | payment_request.payment_status_updated **[LIVE]** | rago-jobs dunning.rs (aggregate status after charging) |
//! | plan.created **[LIVE]** | rago-graphql mutation.rs |
//! | plan.deleted **[LIVE]** | rago-graphql admin_mutations.rs |
//! | plan.updated **[LIVE]** | rago-graphql admin_mutations.rs |
//! | feature.created **[LIVE]** | rago-api resources.rs |
//! | feature.updated **[LIVE]** | rago-api resources.rs |
//! | feature.deleted **[LIVE]** | rago-api resources.rs |
//! | quote.created **[LIVE]** | rago-graphql admin_mutations5.rs |
//! | quote.approved **[LIVE]** | rago-graphql admin_mutations5.rs |
//! | quote.voided **[LIVE]** | rago-graphql admin_mutations5.rs |
//! | subscription.canceled **[LIVE]** | rago-api handlers.rs + rago-graphql mutation.rs (terminating a pending subscription) |
//! | subscription.incomplete | no-op until incomplete-activation path |
//! | subscription.terminated **[LIVE]** | rago-jobs clock/subscriptions.rs (`terminate_ended_subscriptions`) |
//! | subscription.started **[LIVE]** | rago-jobs clock/subscriptions.rs (`activate_subscriptions`) |
//! | subscription.termination_alert **[LIVE]** | rago-jobs clock/subscriptions.rs (`subscriptions_to_be_terminated`) |
//! | subscription.trial_ended **[LIVE]** | rago-jobs clock/subscriptions.rs (`free_trial_biller`) |
//! | subscription.updated **[LIVE]** | rago-graphql admin_mutations.rs |
//! | subscription.usage_threshold_reached | no-op until `UsageMonitoring::CheckThresholdsService` |
//! | wallet.created **[LIVE]** | rago-api handlers.rs (`create_wallet`) |
//! | wallet.updated **[LIVE]** | rago-api crud.rs (`update_wallet`) |
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
    WebhookKind::InvoiceGenerated,         // finalize_invoices + scheduler billing cycle
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
    WebhookKind::PaymentRequestPaymentStatusUpdated,
    WebhookKind::PaymentRequestPaymentFailure,
    // refunds.rs
    WebhookKind::CreditNoteRefundFailure,
];

/// Env var holding the public base URL of this Rago deployment (e.g.
/// `https://billing.example.com`). Used to build absolute `file_url`s in
/// `*.generated` webhooks; without it the URL is the API-relative path.
pub const PUBLIC_URL_ENV: &str = "RAGO_PUBLIC_URL";

/// The download URL of a rendered document: `resource` is `invoices` or
/// `credit_notes`. Downloads are served on demand by the API (authenticated
/// with the org's API key), so the URL is stable for the document's lifetime.
pub fn document_url(resource: &str, id: Uuid) -> String {
    let path = format!("/api/v1/{resource}/{id}/download");
    match std::env::var(PUBLIC_URL_ENV).ok().filter(|v| !v.trim().is_empty()) {
        Some(base) => format!("{}{path}", base.trim_end_matches('/')),
        None => path,
    }
}

/// Emit `invoice.generated` for an invoice whose document is now available
/// (it has been finalized). `object` is the invoice payload; `file_url` is
/// added.
pub async fn record_invoice_generated(
    db: &Db,
    organization_id: Uuid,
    invoice_id: Uuid,
    object: Value,
) -> Result<bool, JobError> {
    record_webhook(db, organization_id, WebhookKind::InvoiceGenerated, with_file_url(object, "invoices", invoice_id)).await
}

/// Emit `credit_note.generated` for a newly issued credit note.
pub async fn record_credit_note_generated(
    db: &Db,
    organization_id: Uuid,
    credit_note_id: Uuid,
    object: Value,
) -> Result<bool, JobError> {
    record_webhook(
        db,
        organization_id,
        WebhookKind::CreditNoteGenerated,
        with_file_url(object, "credit_notes", credit_note_id),
    )
    .await
}

fn with_file_url(object: Value, resource: &str, id: Uuid) -> Value {
    let mut map = match object {
        Value::Object(m) => m,
        other => {
            let mut m = serde_json::Map::new();
            m.insert("object".into(), other);
            m
        }
    };
    map.insert("lago_id".into(), Value::String(id.to_string()));
    map.insert("file_url".into(), Value::String(document_url(resource, id)));
    Value::Object(map)
}

/// Record a pending outbound webhook for a job trigger point.
///
/// Faithful to Lago: a `SendWebhookJob` materializes one `Webhook` per
/// configured `webhook_endpoint`. With no endpoint configured the org receives
/// nothing, so this is a **no-op** (returns `Ok(false)`) rather than writing an
/// undeliverable row. For each endpoint a work item is stored `pending` with the
/// full signed-ready envelope built by [`WebhookKind::build`], plus a linked row
/// in the operator-facing deliveries feed; the delivery worker
/// ([`crate::queue::redeliver_due_webhooks`]) sends it, retries it with backoff,
/// and updates both.
///
/// Returns `true` when at least one webhook row was written, `false` on the
/// no-endpoint no-op.
pub async fn record_webhook(
    db: &Db,
    organization_id: Uuid,
    kind: WebhookKind,
    object: Value,
) -> Result<bool, JobError> {
    let endpoints = db.webhook_endpoints_for_org(organization_id).await?;
    if endpoints.is_empty() {
        return Ok(false);
    }

    for (endpoint_id, endpoint) in endpoints {
        let webhook = kind.build(organization_id, endpoint, object.clone());
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

        // Operator-facing feed row, linked to the work item so the delivery
        // worker can report the real outcome on it. Best-effort: the work item
        // above is what gets delivered.
        if let Ok(delivery_id) = db
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
            .await
        {
            let _ = db.link_webhook_delivery(delivery_id, webhook.id).await;
        }
    }
    Ok(true)
}
