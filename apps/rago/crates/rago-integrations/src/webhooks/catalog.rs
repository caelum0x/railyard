//! The full Lago webhook catalog — faithful port of
//! `SendWebhookJob::WEBHOOK_SERVICES` (`app/jobs/send_webhook_job.rb`) and every
//! `Webhooks::*` service's `webhook_type` / `object_type` pair.
//!
//! Each [`WebhookKind`] maps to the exact `webhook_type` string a receiver sees
//! and the `object_type` key under which the serialized object is nested in the
//! [`super::Webhook`] envelope. [`WebhookKind::build`] is the single per-type
//! emit entry point: it wraps an already-serialized object `Value` (for plain
//! model kinds, produced by the model's V1 serializer in `rago-graphql`/
//! `rago-db`; for bespoke kinds, produced by [`super::payloads`]) into the
//! signed-ready envelope with the correct pair.
//!
//! # Call-site MAP — where each webhook SHOULD be emitted in Rago
//!
//! The Lago trigger service is listed for each type; the wiring is a mechanical
//! follow-up in the owning crate (call `kind.build(...)` then hand the
//! [`super::Webhook`] to `rago_jobs::webhook_delivery::emit_webhook` /
//! `SendHttpWebhookJob`). Types already emitted by Rago today are marked `[LIVE]`.
//!
//! | webhook_type | Lago trigger | Rago emit site (follow-up) |
//! |---|---|---|
//! | alert.triggered `[LIVE]` | `UsageMonitoring::ProcessAlertService` | rago-jobs alert processing |
//! | billable_metric.created/updated/deleted | `BillableMetrics::{Create,Update,Destroy}Service` | rago-graphql/rago-billing billable-metric mutations |
//! | dunning_campaign.finished | `DunningCampaigns::*` completion | rago-jobs dunning finalize |
//! | invoice.created `[LIVE]` | `Invoices::CreateService` & advance-charge/progressive/finalize services | rago-jobs::scheduler (already) + rago-billing invoice create |
//! | invoice.one_off_created `[LIVE]` | `Invoices::CreateOneOffService` | rago-billing one-off invoice |
//! | invoice.paid_credit_added | `Credits::AppliedPrepaidCreditService` / paid_credit | rago-billing prepaid credit apply |
//! | invoice.generated | `Invoices::GeneratePdfService` | rago-jobs pdf generation |
//! | invoice.drafted | `Invoices::RefreshDraftService` | rago-billing draft refresh |
//! | invoice.ready_to_finalize | `Invoices::PullTaxesAndApplyService` | rago-billing tax pull |
//! | invoice.voided | `Invoices::VoidService` | rago-billing void |
//! | invoice.deleted | `Invoices::Payments::DeleteService`/delete | rago-billing invoice delete |
//! | invoice.payment_dispute_lost | `Invoices::LoseDisputeService` | rago-billing dispute |
//! | invoice.payment_status_updated `[LIVE]` | `Invoices::UpdateService` | rago-api::webhooks (already) |
//! | invoice.payment_overdue | `Invoices::MarkOverdueService` | rago-jobs overdue clock |
//! | invoice.payment_failure | `PaymentProviders::DeliverErrorWebhookService` | rago-integrations payment failure path |
//! | invoice.resynced | `Integrations::Aggregator::SyncService` | rago-integrations accounting sync |
//! | event.error | `Events::PostProcessService` | rago-jobs event post-process |
//! | events.errors | `Events::PostValidationService` | rago-jobs event validation |
//! | fee.created | `Fees::CreatePayInAdvance*Service` | rago-billing pay-in-advance fee |
//! | fee.tax_provider_error | `Integrations::Aggregator::Taxes::*` fee error | rago-integrations tax fee error |
//! | customer.created `[LIVE~]` | `Customers::CreateService`/`UpsertFromApi` | rago-billing customer create |
//! | customer.updated `[LIVE]` | `Customers::UpdateService`/`UpsertFromApi` | rago-billing customer update |
//! | customer.accounting_provider_created | `Integrations::Aggregator::Contacts::CreateService` | rago-integrations accounting sync |
//! | customer.accounting_provider_error | accounting contact error | rago-integrations accounting error |
//! | customer.crm_provider_created | CRM contact create | rago-integrations crm sync |
//! | customer.crm_provider_error | CRM contact error | rago-integrations crm error |
//! | customer.payment_provider_created | `PaymentProviderCustomers::*Service` | rago-integrations provider customer create |
//! | customer.payment_provider_error | provider customer error | rago-integrations provider error |
//! | customer.checkout_url_generated | `PaymentProviderCustomers::*::GenerateCheckoutUrlService` | rago-integrations checkout url |
//! | customer.tax_provider_error | tax customer error | rago-integrations tax error |
//! | customer.vies_check | `Customers::EuAutoTaxesService` | rago-integrations vies check |
//! | credit_note.created | `CreditNotes::CreateService` | rago-billing credit note create |
//! | credit_note.generated | `CreditNotes::GeneratePdfService` | rago-jobs pdf generation |
//! | credit_note.refund_failure | `CreditNotes::Refunds::*` failure | rago-integrations refund failure |
//! | integration.provider_error | `Integrations::Aggregator::*` provider error | rago-integrations provider error |
//! | order.created / order.executed | `Orders::*` | **DEFERRED — no orders table in Rago** |
//! | order_form.created/signed/expired/voided | `OrderForms::*` | **DEFERRED — no order_forms table in Rago** |
//! | payment.requires_action | `Payments::CreateService` (3DS) | rago-integrations payment create |
//! | payment.succeeded | provider `*Service` payment success | rago-integrations payment success |
//! | payment_provider.error | `PaymentProviders::*` generic error | rago-integrations provider error |
//! | payment_receipt.created / generated | `PaymentReceipts::*` | **DEFERRED — no payment_receipts table in Rago** |
//! | payment_request.created | `PaymentRequests::CreateService` | rago-billing payment request |
//! | payment_request.payment_failure | `PaymentProviders::DeliverErrorWebhookService` | rago-integrations payment failure |
//! | payment_request.payment_status_updated | `PaymentRequests::UpdateService` | rago-billing payment request update |
//! | plan.created/deleted/updated | `Plans::{Create,Destroy,Update}Service` | rago-graphql/rago-billing plan mutations |
//! | feature.created/updated/deleted | `Entitlement::*Feature*Service` | rago-graphql feature mutations |
//! | quote.created/approved/voided | `Quotes::*` | rago-billing quote lifecycle (quotes table exists) |
//! | subscription.started `[LIVE]` | `Subscriptions::{Create,Activate}Service` | rago-billing subscription create |
//! | subscription.incomplete | `Subscriptions::ActivateService` | rago-billing subscription activate |
//! | subscription.terminated | `Subscriptions::TerminateService` | rago-billing subscription terminate |
//! | subscription.termination_alert `[LIVE]` | `SubscriptionsToBeTerminatedJob` | rago-jobs::clock::subscriptions (already) |
//! | subscription.trial_ended `[LIVE]` | `Subscriptions::FreeTrialBillingService` | rago-jobs trial clock |
//! | subscription.updated | `Subscriptions::PlanDowngradeService`/update | rago-billing subscription update |
//! | subscription.usage_threshold_reached | `UsageMonitoring::CheckThresholdsService` | rago-jobs usage threshold |
//! | wallet.created/updated/terminated | `Wallets::{Create,Update,Terminate}Service` | rago-billing wallet lifecycle |
//! | wallet.depleted_ongoing_balance | `Wallets::Balance::UpdateOngoingService` | rago-jobs wallet balance |
//! | wallet_transaction.created | `WalletTransactions::CreateFromParamsService` | rago-billing wallet txn create |
//! | wallet_transaction.updated | `WalletTransactions::{Settle,MarkAsFailed}Service` | rago-billing wallet txn settle |
//! | wallet_transaction.payment_failure | `PaymentProviders::DeliverErrorWebhookService` | rago-integrations payment failure |

use serde_json::Value;
use uuid::Uuid;

use super::Webhook;

/// Every webhook Lago can emit (`SendWebhookJob::WEBHOOK_SERVICES`).
///
/// The variant order mirrors the Lago registry grouping. Use
/// [`WebhookKind::webhook_type`] / [`WebhookKind::object_type`] for the wire
/// strings and [`WebhookKind::build`] to assemble the envelope.
///
/// Variants marked **no-op** have their type and envelope defined for parity but
/// have no live trigger call-site in Rago yet. They will emit once the noted
/// service/path is implemented — do NOT add fabricated emissions before then.
/// The full trigger map is in `rago-jobs/src/webhook_emit.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebhookKind {
    // ── usage monitoring ────────────────────────────────────────────────────
    /// Fired by rago-api `evaluate_usage_alerts` / `evaluate_lifetime_alerts`.
    AlertTriggered,

    // ── billable metrics ────────────────────────────────────────────────────
    /// Fired by rago-graphql `create_billable_metric`.
    BillableMetricCreated,
    /// Fired by rago-graphql `update_billable_metric`.
    BillableMetricUpdated,
    /// Fired by rago-graphql `delete_billable_metric`.
    BillableMetricDeleted,

    // ── dunning ─────────────────────────────────────────────────────────────
    /// Fired by rago-jobs dunning sweep when all currencies are exhausted.
    DunningCampaignFinished,

    // ── invoices ────────────────────────────────────────────────────────────
    /// Fired by rago-jobs `finalize_invoices` clock and `scheduler` billing cycle.
    InvoiceCreated,
    /// no-op until `Invoices::CreateOneOffService` is implemented.
    InvoiceOneOffCreated,
    /// no-op until `Credits::AppliedPrepaidCreditService` is implemented.
    InvoicePaidCreditAdded,
    /// Fired when an invoice is finalized (document downloadable; carries `file_url`).
    InvoiceGenerated,
    /// Fired by rago-jobs `refresh_draft_invoices` clock.
    InvoiceDrafted,
    /// no-op until `Invoices::PullTaxesAndApplyService` (tax pull) is implemented.
    InvoiceReadyToFinalize,
    /// no-op until `Invoices::VoidService` is implemented.
    InvoiceVoided,
    /// no-op until `Invoices::Payments::DeleteService` is implemented.
    InvoiceDeleted,
    /// no-op until `Invoices::LoseDisputeService` (payment dispute) is implemented.
    InvoicePaymentDisputeLost,
    /// Fired by rago-jobs `scheduler` (post-charge) and rago-api Stripe webhook handler.
    InvoicePaymentStatusUpdated,
    /// Fired by rago-jobs `mark_invoices_as_payment_overdue` clock.
    InvoicePaymentOverdue,
    /// no-op until `PaymentProviders::DeliverErrorWebhookService` (payment failure) is implemented.
    InvoicePaymentFailure,
    /// no-op until `Integrations::Aggregator::SyncService` (accounting resync) is implemented.
    InvoiceResynced,

    // ── events ──────────────────────────────────────────────────────────────
    /// no-op until `Events::PostProcessService` (event post-process) is implemented.
    EventError,
    /// no-op until `Events::PostValidationService` (event batch validation) is implemented.
    EventsErrors,

    // ── fees ────────────────────────────────────────────────────────────────
    /// Fired by rago-jobs `scheduler` billing cycle (one per assembled fee).
    FeeCreated,
    /// no-op until `Integrations::Aggregator::Taxes` fee error path is implemented.
    FeeTaxProviderError,

    // ── customers ───────────────────────────────────────────────────────────
    /// Fired by rago-api `upsert_customer` (create path).
    CustomerCreated,
    /// Fired by rago-api `upsert_customer` (update path).
    CustomerUpdated,
    /// no-op until `Integrations::Aggregator::Contacts::CreateService` is implemented.
    CustomerAccountingProviderCreated,
    /// no-op until accounting contact error path is implemented.
    CustomerAccountingProviderError,
    /// no-op until CRM contact create path is implemented.
    CustomerCrmProviderCreated,
    /// no-op until CRM contact error path is implemented.
    CustomerCrmProviderError,
    /// no-op until `PaymentProviderCustomers::*Service` create path is implemented.
    CustomerPaymentProviderCreated,
    /// no-op until payment provider customer error path is implemented.
    CustomerPaymentProviderError,
    /// no-op until `PaymentProviderCustomers::*::GenerateCheckoutUrlService` is implemented.
    CustomerCheckoutUrlGenerated,
    /// no-op until tax provider customer error path is implemented.
    CustomerTaxProviderError,
    /// no-op until `Customers::EuAutoTaxesService` (VIES check) is implemented.
    CustomerViesCheck,

    // ── credit notes ────────────────────────────────────────────────────────
    /// Fired by rago-api `create_credit_note` and rago-graphql `create_credit_note`.
    CreditNoteCreated,
    /// Fired after credit-note creation (carries `file_url`).
    CreditNoteGenerated,
    /// Fired by rago-jobs `refunds` when the provider declines a credit-note refund.
    CreditNoteRefundFailure,

    // ── integrations ────────────────────────────────────────────────────────
    /// no-op until `Integrations::Aggregator::*` provider error path is implemented.
    IntegrationProviderError,

    // ── orders ──────────────────────────────────────────────────────────────
    /// no-op until orders schema is implemented (`is_schema_deferred`).
    OrderCreated,
    /// Fired by rago-graphql `execute_order`.
    OrderExecuted,
    /// no-op until order_forms schema is implemented (`is_schema_deferred`).
    OrderFormCreated,
    /// Fired by rago-graphql `sign_order_form`.
    OrderFormSigned,
    /// no-op until order_forms schema is implemented (`is_schema_deferred`).
    OrderFormExpired,
    /// Fired by rago-graphql `void_order_form`.
    OrderFormVoided,

    // ── payments ────────────────────────────────────────────────────────────
    /// no-op until `Payments::CreateService` (3DS / SCA flow) is implemented.
    PaymentRequiresAction,
    /// no-op until provider payment success webhook path is implemented.
    PaymentSucceeded,
    /// no-op until `PaymentProviders::*` generic error path is implemented.
    PaymentProviderError,

    // ── payment receipts ────────────────────────────────────────────────────
    /// no-op until payment_receipts schema is implemented (`is_schema_deferred`).
    PaymentReceiptCreated,
    /// no-op until payment_receipts schema is implemented (`is_schema_deferred`).
    PaymentReceiptGenerated,

    // ── payment requests ────────────────────────────────────────────────────
    /// Fired by rago-jobs dunning sweep (`create_and_charge_payment_request`).
    PaymentRequestCreated,
    /// Fired by rago-jobs `dunning` when an invoice of a payment request cannot be paid.
    PaymentRequestPaymentFailure,
    /// Fired by rago-jobs `dunning` with the payment request's aggregate status.
    PaymentRequestPaymentStatusUpdated,

    // ── plans ───────────────────────────────────────────────────────────────
    /// Fired by rago-graphql `create_plan`.
    PlanCreated,
    /// Fired by rago-graphql `delete_plan`.
    PlanDeleted,
    /// Fired by rago-graphql `update_plan`.
    PlanUpdated,

    // ── features (entitlements) ─────────────────────────────────────────────
    /// Fired by rago-api `create_feature`.
    FeatureCreated,
    /// Fired by rago-api `update_feature`.
    FeatureUpdated,
    /// Fired by rago-api `delete_feature`.
    FeatureDeleted,

    // ── quotes ──────────────────────────────────────────────────────────────
    /// Fired by rago-graphql `create_quote`.
    QuoteCreated,
    /// Fired by rago-graphql `approve_quote`.
    QuoteApproved,
    /// Fired by rago-graphql `void_quote`.
    QuoteVoided,

    // ── subscriptions ───────────────────────────────────────────────────────
    /// Fired when a pending (never started) subscription is terminated.
    SubscriptionCanceled,
    /// no-op until incomplete-activation path (`Subscriptions::ActivateService` failure) is implemented.
    SubscriptionIncomplete,
    /// Fired by rago-jobs `terminate_ended_subscriptions` clock.
    SubscriptionTerminated,
    /// Fired by rago-jobs `activate_subscriptions` clock.
    SubscriptionStarted,
    /// Fired by rago-jobs `subscriptions_to_be_terminated` clock (15/45-day alert).
    SubscriptionTerminationAlert,
    /// Fired by rago-jobs `free_trial_biller` clock.
    SubscriptionTrialEnded,
    /// Fired by rago-graphql `update_subscription`.
    SubscriptionUpdated,
    /// no-op until `UsageMonitoring::CheckThresholdsService` is implemented.
    SubscriptionUsageThresholdReached,

    // ── wallets ─────────────────────────────────────────────────────────────
    /// no-op until `Wallets::CreateService` is implemented.
    WalletCreated,
    /// no-op until `Wallets::UpdateService` is implemented.
    WalletUpdated,
    /// Fired by rago-jobs `terminate_wallets` clock.
    WalletTerminated,
    /// Fired by rago-jobs `refresh_wallets_ongoing_balance` clock (depletion transition).
    WalletDepletedOngoingBalance,

    // ── wallet transactions ──────────────────────────────────────────────────
    /// Fired by rago-jobs `create_interval_wallet_transactions` clock.
    WalletTransactionCreated,
    /// no-op until `WalletTransactions::{Settle,MarkAsFailed}Service` is implemented.
    WalletTransactionUpdated,
    /// no-op until `PaymentProviders::DeliverErrorWebhookService` (wallet txn failure) is implemented.
    WalletTransactionPaymentFailure,
}

impl WebhookKind {
    /// All catalog entries, in registry order.
    pub const ALL: [WebhookKind; 75] = [
        WebhookKind::AlertTriggered,
        WebhookKind::BillableMetricCreated,
        WebhookKind::BillableMetricUpdated,
        WebhookKind::BillableMetricDeleted,
        WebhookKind::DunningCampaignFinished,
        WebhookKind::InvoiceCreated,
        WebhookKind::InvoiceOneOffCreated,
        WebhookKind::InvoicePaidCreditAdded,
        WebhookKind::InvoiceGenerated,
        WebhookKind::InvoiceDrafted,
        WebhookKind::InvoiceReadyToFinalize,
        WebhookKind::InvoiceVoided,
        WebhookKind::InvoiceDeleted,
        WebhookKind::InvoicePaymentDisputeLost,
        WebhookKind::InvoicePaymentStatusUpdated,
        WebhookKind::InvoicePaymentOverdue,
        WebhookKind::InvoicePaymentFailure,
        WebhookKind::InvoiceResynced,
        WebhookKind::EventError,
        WebhookKind::EventsErrors,
        WebhookKind::FeeCreated,
        WebhookKind::FeeTaxProviderError,
        WebhookKind::CustomerCreated,
        WebhookKind::CustomerUpdated,
        WebhookKind::CustomerAccountingProviderCreated,
        WebhookKind::CustomerAccountingProviderError,
        WebhookKind::CustomerCrmProviderCreated,
        WebhookKind::CustomerCrmProviderError,
        WebhookKind::CustomerPaymentProviderCreated,
        WebhookKind::CustomerPaymentProviderError,
        WebhookKind::CustomerCheckoutUrlGenerated,
        WebhookKind::CustomerTaxProviderError,
        WebhookKind::CustomerViesCheck,
        WebhookKind::CreditNoteCreated,
        WebhookKind::CreditNoteGenerated,
        WebhookKind::CreditNoteRefundFailure,
        WebhookKind::IntegrationProviderError,
        WebhookKind::OrderCreated,
        WebhookKind::OrderExecuted,
        WebhookKind::OrderFormCreated,
        WebhookKind::OrderFormSigned,
        WebhookKind::OrderFormExpired,
        WebhookKind::OrderFormVoided,
        WebhookKind::PaymentRequiresAction,
        WebhookKind::PaymentSucceeded,
        WebhookKind::PaymentProviderError,
        WebhookKind::PaymentReceiptCreated,
        WebhookKind::PaymentReceiptGenerated,
        WebhookKind::PaymentRequestCreated,
        WebhookKind::PaymentRequestPaymentFailure,
        WebhookKind::PaymentRequestPaymentStatusUpdated,
        WebhookKind::PlanCreated,
        WebhookKind::PlanDeleted,
        WebhookKind::PlanUpdated,
        WebhookKind::FeatureCreated,
        WebhookKind::FeatureUpdated,
        WebhookKind::FeatureDeleted,
        WebhookKind::QuoteCreated,
        WebhookKind::QuoteApproved,
        WebhookKind::QuoteVoided,
        WebhookKind::SubscriptionCanceled,
        WebhookKind::SubscriptionIncomplete,
        WebhookKind::SubscriptionTerminated,
        WebhookKind::SubscriptionStarted,
        WebhookKind::SubscriptionTerminationAlert,
        WebhookKind::SubscriptionTrialEnded,
        WebhookKind::SubscriptionUpdated,
        WebhookKind::SubscriptionUsageThresholdReached,
        WebhookKind::WalletCreated,
        WebhookKind::WalletUpdated,
        WebhookKind::WalletTerminated,
        WebhookKind::WalletDepletedOngoingBalance,
        WebhookKind::WalletTransactionCreated,
        WebhookKind::WalletTransactionUpdated,
        WebhookKind::WalletTransactionPaymentFailure,
    ];

    /// The `webhook_type` string a receiver sees (the service's `webhook_type`).
    pub const fn webhook_type(self) -> &'static str {
        use WebhookKind::*;
        match self {
            AlertTriggered => "alert.triggered",
            BillableMetricCreated => "billable_metric.created",
            BillableMetricUpdated => "billable_metric.updated",
            BillableMetricDeleted => "billable_metric.deleted",
            DunningCampaignFinished => "dunning_campaign.finished",
            InvoiceCreated => "invoice.created",
            InvoiceOneOffCreated => "invoice.one_off_created",
            InvoicePaidCreditAdded => "invoice.paid_credit_added",
            InvoiceGenerated => "invoice.generated",
            InvoiceDrafted => "invoice.drafted",
            InvoiceReadyToFinalize => "invoice.ready_to_finalize",
            InvoiceVoided => "invoice.voided",
            InvoiceDeleted => "invoice.deleted",
            InvoicePaymentDisputeLost => "invoice.payment_dispute_lost",
            InvoicePaymentStatusUpdated => "invoice.payment_status_updated",
            InvoicePaymentOverdue => "invoice.payment_overdue",
            InvoicePaymentFailure => "invoice.payment_failure",
            InvoiceResynced => "invoice.resynced",
            EventError => "event.error",
            EventsErrors => "events.errors",
            FeeCreated => "fee.created",
            FeeTaxProviderError => "fee.tax_provider_error",
            CustomerCreated => "customer.created",
            CustomerUpdated => "customer.updated",
            CustomerAccountingProviderCreated => "customer.accounting_provider_created",
            CustomerAccountingProviderError => "customer.accounting_provider_error",
            CustomerCrmProviderCreated => "customer.crm_provider_created",
            CustomerCrmProviderError => "customer.crm_provider_error",
            CustomerPaymentProviderCreated => "customer.payment_provider_created",
            CustomerPaymentProviderError => "customer.payment_provider_error",
            CustomerCheckoutUrlGenerated => "customer.checkout_url_generated",
            CustomerTaxProviderError => "customer.tax_provider_error",
            CustomerViesCheck => "customer.vies_check",
            CreditNoteCreated => "credit_note.created",
            CreditNoteGenerated => "credit_note.generated",
            CreditNoteRefundFailure => "credit_note.refund_failure",
            IntegrationProviderError => "integration.provider_error",
            OrderCreated => "order.created",
            OrderExecuted => "order.executed",
            OrderFormCreated => "order_form.created",
            OrderFormSigned => "order_form.signed",
            OrderFormExpired => "order_form.expired",
            OrderFormVoided => "order_form.voided",
            PaymentRequiresAction => "payment.requires_action",
            PaymentSucceeded => "payment.succeeded",
            PaymentProviderError => "payment_provider.error",
            PaymentReceiptCreated => "payment_receipt.created",
            PaymentReceiptGenerated => "payment_receipt.generated",
            PaymentRequestCreated => "payment_request.created",
            PaymentRequestPaymentFailure => "payment_request.payment_failure",
            PaymentRequestPaymentStatusUpdated => "payment_request.payment_status_updated",
            PlanCreated => "plan.created",
            PlanDeleted => "plan.deleted",
            PlanUpdated => "plan.updated",
            FeatureCreated => "feature.created",
            FeatureUpdated => "feature.updated",
            FeatureDeleted => "feature.deleted",
            QuoteCreated => "quote.created",
            QuoteApproved => "quote.approved",
            QuoteVoided => "quote.voided",
            SubscriptionCanceled => "subscription.canceled",
            SubscriptionIncomplete => "subscription.incomplete",
            SubscriptionTerminated => "subscription.terminated",
            SubscriptionStarted => "subscription.started",
            SubscriptionTerminationAlert => "subscription.termination_alert",
            SubscriptionTrialEnded => "subscription.trial_ended",
            SubscriptionUpdated => "subscription.updated",
            SubscriptionUsageThresholdReached => "subscription.usage_threshold_reached",
            WalletCreated => "wallet.created",
            WalletUpdated => "wallet.updated",
            WalletTerminated => "wallet.terminated",
            WalletDepletedOngoingBalance => "wallet.depleted_ongoing_balance",
            WalletTransactionCreated => "wallet_transaction.created",
            WalletTransactionUpdated => "wallet_transaction.updated",
            WalletTransactionPaymentFailure => "wallet_transaction.payment_failure",
        }
    }

    /// The `object_type` — the payload key under which the object nests, and the
    /// serializer `root_name`. Differs from the resource for bespoke error /
    /// checkout envelopes (e.g. `invoice.payment_failure` nests under
    /// `payment_provider_invoice_payment_error`).
    pub const fn object_type(self) -> &'static str {
        use WebhookKind::*;
        match self {
            AlertTriggered => "triggered_alert",
            BillableMetricCreated | BillableMetricUpdated | BillableMetricDeleted => {
                "billable_metric"
            }
            DunningCampaignFinished => "dunning_campaign",
            InvoiceCreated
            | InvoiceOneOffCreated
            | InvoicePaidCreditAdded
            | InvoiceGenerated
            | InvoiceDrafted
            | InvoiceReadyToFinalize
            | InvoiceVoided
            | InvoiceDeleted
            | InvoicePaymentStatusUpdated
            | InvoicePaymentOverdue
            | InvoiceResynced => "invoice",
            InvoicePaymentDisputeLost => "payment_dispute_lost",
            InvoicePaymentFailure => "payment_provider_invoice_payment_error",
            EventError => "event_error",
            EventsErrors => "events_errors",
            FeeCreated => "fee",
            FeeTaxProviderError => "tax_provider_fee_error",
            CustomerCreated
            | CustomerUpdated
            | CustomerAccountingProviderCreated
            | CustomerCrmProviderCreated
            | CustomerPaymentProviderCreated
            | CustomerViesCheck => "customer",
            CustomerAccountingProviderError => "accounting_provider_customer_error",
            CustomerCrmProviderError => "crm_provider_customer_error",
            CustomerPaymentProviderError => "payment_provider_customer_error",
            CustomerCheckoutUrlGenerated => "payment_provider_customer_checkout_url",
            CustomerTaxProviderError => "tax_provider_customer_error",
            CreditNoteCreated | CreditNoteGenerated => "credit_note",
            CreditNoteRefundFailure => "credit_note_payment_provider_refund_error",
            IntegrationProviderError => "provider_error",
            OrderCreated | OrderExecuted => "order",
            OrderFormCreated | OrderFormSigned | OrderFormExpired | OrderFormVoided => "order_form",
            PaymentRequiresAction | PaymentSucceeded => "payment",
            PaymentProviderError => "payment_provider_error",
            PaymentReceiptCreated | PaymentReceiptGenerated => "payment_receipt",
            PaymentRequestCreated | PaymentRequestPaymentStatusUpdated => "payment_request",
            PaymentRequestPaymentFailure => "payment_provider_payment_request_payment_error",
            PlanCreated | PlanDeleted | PlanUpdated => "plan",
            FeatureCreated | FeatureUpdated | FeatureDeleted => "feature",
            QuoteCreated | QuoteApproved | QuoteVoided => "quote",
            SubscriptionCanceled
            | SubscriptionIncomplete
            | SubscriptionTerminated
            | SubscriptionStarted
            | SubscriptionTerminationAlert
            | SubscriptionTrialEnded
            | SubscriptionUpdated
            | SubscriptionUsageThresholdReached => "subscription",
            WalletCreated | WalletUpdated | WalletTerminated | WalletDepletedOngoingBalance => {
                "wallet"
            }
            WalletTransactionCreated | WalletTransactionUpdated => "wallet_transaction",
            WalletTransactionPaymentFailure => "payment_provider_wallet_transaction_payment_error",
        }
    }

    /// `true` if emit-wiring is deferred because Rago has no matching schema
    /// (orders, order_forms, payment_receipts). The type constant + envelope are
    /// still defined for parity; only the trigger call-site is pending.
    pub const fn is_schema_deferred(self) -> bool {
        use WebhookKind::*;
        matches!(
            self,
            OrderCreated
                | OrderExecuted
                | OrderFormCreated
                | OrderFormSigned
                | OrderFormExpired
                | OrderFormVoided
                | PaymentReceiptCreated
                | PaymentReceiptGenerated
        )
    }

    /// Resolve a `webhook_type` wire string to its [`WebhookKind`].
    pub fn from_webhook_type(s: &str) -> Option<WebhookKind> {
        WebhookKind::ALL
            .into_iter()
            .find(|k| k.webhook_type() == s)
    }

    /// The single per-type emit entry point: wrap an already-serialized `object`
    /// into the signed-ready [`Webhook`] envelope with this kind's
    /// `webhook_type`/`object_type`. For bespoke error/checkout kinds, build
    /// `object` with the matching [`super::payloads`] helper first.
    pub fn build(self, organization_id: Uuid, endpoint: impl Into<String>, object: Value) -> Webhook {
        Webhook::build(
            organization_id,
            endpoint,
            self.webhook_type(),
            self.object_type(),
            object,
        )
    }
}
