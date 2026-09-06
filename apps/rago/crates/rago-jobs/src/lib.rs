//! # rago-jobs
//!
//! Background job + scheduler runtime for Rago.
//!
//! * [`bill_subscription`] — the in-arrears billing domain job
//!   (`BillSubscriptionJob` / `Invoices::SubscriptionService`).
//! * [`scheduler`] — the auto-biller (`Clock::SubscriptionsBillerJob` →
//!   `OrganizationBillingService`): find subscriptions due at their period
//!   boundary, bill → invoice, emit `invoice.created`, charge the customer,
//!   emit the payment-status webhook.
//! * [`payments`] — charge an invoice through a provider and update its
//!   `payment_status` (`Invoices::Payments::CreateService`).
//! * [`webhook_delivery`] — build/sign/deliver/persist an outbound webhook
//!   with retry (`Webhooks::SendHttpService`).
//! * [`webhook_emit`] — record a *pending* webhook at a clock/job trigger point
//!   (`SendWebhookJob.perform_later`), gated on a configured endpoint.
//!
//! Provider/webhook network calls go through an injectable
//! [`rago_integrations::HttpTransport`], so the whole pipeline runs against an
//! in-memory transport in tests and demos.

pub mod bill_subscription;
pub mod clock;
pub mod crons;
pub mod data_export;
pub mod dunning;
pub mod payments;
pub mod queue;
pub mod scheduler;
pub mod webhook_delivery;
pub mod webhook_emit;

pub use bill_subscription::perform as bill_subscription;
pub use clock::{
    run_all as run_all_clocks, run_due as run_due_clocks, Cadence, ClockJobId, ClockReport,
};
pub use crons::{run_dunning, run_subscription_crons, CronReport};
pub use data_export::JOB_DATA_EXPORT;
pub use dunning::{process_dunning_campaigns, DunningConfig, DunningReport};
pub use queue::{
    drain, enqueue_billing_cycle, redeliver_due_webhooks, run_one, JOB_BILL_SUBSCRIPTION,
};
pub use scheduler::{run_billing_cycle, CycleReport, SchedulerConfig};

/// Errors surfaced by the jobs/scheduler runtime.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error(transparent)]
    Db(#[from] rago_db::DbError),
    #[error(transparent)]
    Billing(#[from] rago_billing::error::BillingError),
    #[error("payment error: {0}")]
    Payment(String),
}
