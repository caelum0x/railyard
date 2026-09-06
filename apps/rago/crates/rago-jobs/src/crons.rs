//! `Clock::*` cron sweeps — subscription lifecycle + invoice finalization.
//!
//! Ports `Clock::{ActivateSubscriptions, TerminateEndedSubscriptions,
//! ExpireIncompleteSubscriptions, FreeTrialSubscriptionsBiller,
//! FinalizeInvoices}Job` as idempotent SQL sweeps run each tick.

use chrono::{DateTime, Duration, Utc};
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
}

/// Run the subscription-lifecycle crons once.
pub async fn run_subscription_crons(db: &Db) -> Result<CronReport, JobError> {
    let now = Utc::now().naive_utc();
    // Incomplete subscriptions older than 3 days are expired (Lago default).
    let incomplete_cutoff = (Utc::now() - Duration::days(3)).naive_utc();

    Ok(CronReport {
        activated: db.activate_due_subscriptions(now).await?,
        terminated: db.terminate_ended_subscriptions(now).await?,
        expired_incomplete: db
            .expire_incomplete_subscriptions(incomplete_cutoff)
            .await?,
        free_trials_ended: db.end_free_trials(now).await?,
    })
}

/// `Clock::FinalizeInvoicesJob` — finalize an organization's draft invoices.
pub async fn finalize_invoices(db: &Db, organization_id: uuid::Uuid) -> Result<u64, JobError> {
    Ok(db.finalize_draft_invoices(organization_id).await?)
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
