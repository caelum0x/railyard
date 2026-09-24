//! `Clock::*` cron sweeps — subscription lifecycle + invoice finalization.
//!
//! Ports `Clock::{ActivateSubscriptions, TerminateEndedSubscriptions,
//! ExpireIncompleteSubscriptions, FreeTrialSubscriptionsBiller,
//! FinalizeInvoices}Job` as idempotent SQL sweeps run each tick.

use chrono::{DateTime, Utc};
use rago_db::Db;
use rago_integrations::HttpTransport;

use crate::dunning::{process_dunning_campaigns, DunningConfig, DunningReport};
use crate::JobError;

/// What a cron tick transitioned.
#[derive(Debug, Default)]
pub struct CronReport {
    pub activated: u64,
    pub terminated: u64,
    pub expired_incomplete: u64,
    pub free_trials_ended: u64,
    /// Pay-in-advance subscriptions billed their first period on starting.
    pub first_periods_billed: u64,
}

/// Run the subscription-lifecycle crons once.
///
/// Activation, termination and incomplete expiry go through the clock jobs
/// themselves (not bare bulk UPDATEs), so a transition made on this per-tick
/// path still runs the payment-method check and emits its webhook
/// (`subscription.started` / `.incomplete` / `.terminated` / `.canceled`).
pub async fn run_subscription_crons(db: &Db) -> Result<CronReport, JobError> {
    let now = Utc::now().naive_utc();

    Ok(CronReport {
        activated: crate::clock::subscriptions::activate_subscriptions(db).await?,
        terminated: crate::clock::subscriptions::terminate_ended_subscriptions(db).await?,
        expired_incomplete: crate::clock::subscriptions::expire_incomplete_subscriptions(db).await?,
        free_trials_ended: db.end_free_trials(now).await?,
        first_periods_billed: crate::clock::subscriptions::bill_starting_subscriptions(db).await?,
    })
}

/// `Clock::FinalizeInvoicesJob` — finalize an organization's draft invoices
/// (numbering each, then provider taxes, webhooks and collection).
pub async fn finalize_invoices(db: &Db, organization_id: uuid::Uuid) -> Result<u64, JobError> {
    Ok(crate::finalize::finalize_all_invoices(db, organization_id).await?.len() as u64)
}

/// `Clock::MarkInvoicesAsPaymentOverdueJob` + `Clock::ProcessDunningCampaignsJob`
/// — the failed-payment recovery tick.
///
/// First flags finalized, unpaid invoices past due as overdue
/// (`mark_payment_overdue`), then runs the full campaign sweep over them
/// ([`process_dunning_campaigns`]): evaluate each eligible customer, create +
/// charge a payment request per fired threshold, and emit
/// `dunning_campaign.finished` when a campaign is exhausted. Marking is kept
/// here (in addition to the standalone `MarkInvoicesAsPaymentOverdue` clock job)
/// so the worker/server daemons — which do not run the clock registry — remain
/// self-sufficient.
pub async fn run_dunning<T: HttpTransport>(
    db: &Db,
    transport: &T,
    cfg: &DunningConfig,
) -> Result<DunningReport, JobError> {
    let now: DateTime<Utc> = Utc::now();
    let flagged = db.mark_payment_overdue(now.naive_utc()).await?;
    let mut report = process_dunning_campaigns(db, transport, cfg, now).await?;
    report.invoices_marked_overdue = flagged;
    Ok(report)
}
