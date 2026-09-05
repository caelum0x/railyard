//! `Clock::*` cron sweeps — subscription lifecycle + invoice finalization.
//!
//! Ports `Clock::{ActivateSubscriptions, TerminateEndedSubscriptions,
//! ExpireIncompleteSubscriptions, FreeTrialSubscriptionsBiller,
//! FinalizeInvoices}Job` as idempotent SQL sweeps run each tick.

use chrono::{Duration, Utc};
use rago_db::Db;

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
        expired_incomplete: db.expire_incomplete_subscriptions(incomplete_cutoff).await?,
        free_trials_ended: db.end_free_trials(now).await?,
    })
}

/// `Clock::FinalizeInvoicesJob` — finalize an organization's draft invoices.
pub async fn finalize_invoices(db: &Db, organization_id: uuid::Uuid) -> Result<u64, JobError> {
    Ok(db.finalize_draft_invoices(organization_id).await?)
}

/// `Clock::MarkInvoicesAsPaymentOverdueJob` + `ProcessDunningCampaignsJob` —
/// flag finalized, unpaid invoices as overdue for dunning. Returns the number
/// newly flagged.
pub async fn run_dunning(db: &Db) -> Result<u64, JobError> {
    Ok(db.mark_payment_overdue(Utc::now().naive_utc()).await?)
}
