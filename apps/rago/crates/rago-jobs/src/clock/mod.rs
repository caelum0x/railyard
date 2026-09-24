//! `Clock::*` job registry — the billing-automation scheduler.
//!
//! Ports Lago's `config/clock.rb` clockwork schedule: each `Clock::*Job` is a
//! real, idempotent sweep (query → act → persist) exposed as an async function
//! in a submodule, registered here with its Lago cadence, and dispatched by
//! [`run_all`] (run every job once) or [`run_due`] (run only the jobs whose
//! cadence elapsed between two ticks).
//!
//! Every job shares the signature `async fn(&Db) -> Result<u64, JobError>` and
//! returns the number of rows it transitioned, so the registry stays uniform
//! and the whole schedule is table-driven.

pub mod coupons;
pub mod invoices;
pub mod orders;
pub mod subscriptions;
pub mod usage;
pub mod wallets;
pub mod webhooks;

use chrono::{DateTime, Duration, TimeZone, Timelike, Utc};
use rago_db::Db;

use crate::JobError;

/// A job's cadence, mirroring the clockwork `every(...)` schedules in
/// `config/clock.rb`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cadence {
    /// Every N minutes, aligned to the epoch (e.g. `*/5 * * * *`).
    Minutes(u32),
    /// Every hour at minute M (e.g. `M */1 * * *`).
    HourlyAt(u32),
    /// Every day at HH:MM (e.g. `M H * * *`).
    DailyAt(u32, u32),
}

impl Cadence {
    /// A cron-ish description (for logging / the bin banner).
    pub fn cron(&self) -> String {
        match self {
            Cadence::Minutes(m) => format!("*/{m} * * * *"),
            Cadence::HourlyAt(min) => format!("{min} */1 * * *"),
            Cadence::DailyAt(h, m) => format!("{m} {h} * * *"),
        }
    }

    /// The most recent instant this cadence was scheduled to fire at or before
    /// `now`.
    fn last_fire(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        match *self {
            Cadence::Minutes(m) => {
                let step = (m.max(1) as i64) * 60;
                let floored = (now.timestamp() / step) * step;
                Utc.timestamp_opt(floored, 0).single().unwrap_or(now)
            }
            Cadence::HourlyAt(min) => {
                let candidate = now
                    .with_minute(min.min(59))
                    .and_then(|t| t.with_second(0))
                    .and_then(|t| t.with_nanosecond(0))
                    .unwrap_or(now);
                if candidate > now {
                    candidate - Duration::hours(1)
                } else {
                    candidate
                }
            }
            Cadence::DailyAt(h, m) => {
                let candidate = now
                    .with_hour(h.min(23))
                    .and_then(|t| t.with_minute(m.min(59)))
                    .and_then(|t| t.with_second(0))
                    .and_then(|t| t.with_nanosecond(0))
                    .unwrap_or(now);
                if candidate > now {
                    candidate - Duration::days(1)
                } else {
                    candidate
                }
            }
        }
    }

    /// Whether this cadence elapsed in the half-open window `(prev, now]`.
    pub fn is_due(&self, prev: DateTime<Utc>, now: DateTime<Utc>) -> bool {
        let fire = self.last_fire(now);
        fire > prev && fire <= now
    }
}

/// Every clock job Rago runs, in a stable order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClockJobId {
    ActivateSubscriptions,
    RefreshDraftInvoices,
    RefreshLifetimeUsages,
    RefreshWalletsOngoingBalance,
    RetryFailedInvoices,
    InboundWebhooksRetry,
    TerminateEndedSubscriptions,
    ExpireIncompleteSubscriptions,
    FinalizeInvoices,
    ComputeAllDailyUsages,
    MarkInvoicesAsPaymentOverdue,
    RetryGeneratingSubscriptionInvoices,
    TerminateCoupons,
    FreeTrialSubscriptionsBiller,
    TerminateWallets,
    SubscriptionsToBeTerminated,
    TerminateRecurringTransactionRules,
    CreateIntervalWalletTransactions,
    WebhooksCleanup,
    InboundWebhooksCleanup,
    ExpireOrderForms,
    ExecuteScheduledOrders,
}

/// All registered clock jobs, in run order.
pub const ALL_JOBS: [ClockJobId; 22] = [
    ClockJobId::ActivateSubscriptions,
    ClockJobId::RefreshDraftInvoices,
    ClockJobId::RefreshLifetimeUsages,
    ClockJobId::RefreshWalletsOngoingBalance,
    ClockJobId::RetryFailedInvoices,
    ClockJobId::InboundWebhooksRetry,
    ClockJobId::TerminateEndedSubscriptions,
    ClockJobId::ExpireIncompleteSubscriptions,
    ClockJobId::FinalizeInvoices,
    ClockJobId::ComputeAllDailyUsages,
    ClockJobId::MarkInvoicesAsPaymentOverdue,
    ClockJobId::RetryGeneratingSubscriptionInvoices,
    ClockJobId::TerminateCoupons,
    ClockJobId::FreeTrialSubscriptionsBiller,
    ClockJobId::TerminateWallets,
    ClockJobId::SubscriptionsToBeTerminated,
    ClockJobId::TerminateRecurringTransactionRules,
    ClockJobId::CreateIntervalWalletTransactions,
    ClockJobId::WebhooksCleanup,
    ClockJobId::InboundWebhooksCleanup,
    ClockJobId::ExpireOrderForms,
    ClockJobId::ExecuteScheduledOrders,
];

impl ClockJobId {
    /// The Lago job name this mirrors.
    pub fn name(&self) -> &'static str {
        match self {
            ClockJobId::ActivateSubscriptions => "activate_subscriptions",
            ClockJobId::RefreshDraftInvoices => "refresh_draft_invoices",
            ClockJobId::RefreshLifetimeUsages => "refresh_lifetime_usages",
            ClockJobId::RefreshWalletsOngoingBalance => "refresh_wallets_ongoing_balance",
            ClockJobId::RetryFailedInvoices => "retry_failed_invoices",
            ClockJobId::InboundWebhooksRetry => "retry_inbound_webhooks",
            ClockJobId::TerminateEndedSubscriptions => "terminate_ended_subscriptions",
            ClockJobId::ExpireIncompleteSubscriptions => "expire_incomplete_subscriptions",
            ClockJobId::FinalizeInvoices => "finalize_invoices",
            ClockJobId::ComputeAllDailyUsages => "compute_daily_usage",
            ClockJobId::MarkInvoicesAsPaymentOverdue => "mark_invoices_as_payment_overdue",
            ClockJobId::RetryGeneratingSubscriptionInvoices => "retry_generating_subscription_invoices",
            ClockJobId::TerminateCoupons => "terminate_coupons",
            ClockJobId::FreeTrialSubscriptionsBiller => "bill_ended_trial_subscriptions",
            ClockJobId::TerminateWallets => "terminate_wallets",
            ClockJobId::SubscriptionsToBeTerminated => "termination_alert",
            ClockJobId::TerminateRecurringTransactionRules => "terminate_expired_wallet_transaction_rules",
            ClockJobId::CreateIntervalWalletTransactions => "top_up_wallet_interval_credits",
            ClockJobId::WebhooksCleanup => "clean_webhooks",
            ClockJobId::InboundWebhooksCleanup => "clean_inbound_webhooks",
            ClockJobId::ExpireOrderForms => "expire_order_forms",
            ClockJobId::ExecuteScheduledOrders => "execute_scheduled_orders",
        }
    }

    /// The job's Lago cadence (from `config/clock.rb`).
    pub fn cadence(&self) -> Cadence {
        match self {
            ClockJobId::ActivateSubscriptions => Cadence::Minutes(5),
            ClockJobId::RefreshDraftInvoices => Cadence::Minutes(5),
            ClockJobId::RefreshLifetimeUsages => Cadence::Minutes(5),
            ClockJobId::RefreshWalletsOngoingBalance => Cadence::Minutes(5),
            ClockJobId::RetryFailedInvoices => Cadence::Minutes(15),
            ClockJobId::InboundWebhooksRetry => Cadence::Minutes(15),
            ClockJobId::TerminateEndedSubscriptions => Cadence::HourlyAt(5),
            ClockJobId::ExpireIncompleteSubscriptions => Cadence::HourlyAt(20),
            ClockJobId::FinalizeInvoices => Cadence::HourlyAt(20),
            ClockJobId::ComputeAllDailyUsages => Cadence::HourlyAt(15),
            ClockJobId::MarkInvoicesAsPaymentOverdue => Cadence::HourlyAt(25),
            ClockJobId::RetryGeneratingSubscriptionInvoices => Cadence::HourlyAt(30),
            ClockJobId::TerminateCoupons => Cadence::HourlyAt(30),
            ClockJobId::FreeTrialSubscriptionsBiller => Cadence::HourlyAt(35),
            ClockJobId::TerminateWallets => Cadence::HourlyAt(45),
            ClockJobId::SubscriptionsToBeTerminated => Cadence::HourlyAt(50),
            ClockJobId::TerminateRecurringTransactionRules => Cadence::HourlyAt(50),
            ClockJobId::CreateIntervalWalletTransactions => Cadence::HourlyAt(55),
            ClockJobId::WebhooksCleanup => Cadence::DailyAt(1, 0),
            ClockJobId::InboundWebhooksCleanup => Cadence::DailyAt(1, 10),
            ClockJobId::ExpireOrderForms => Cadence::HourlyAt(10),
            ClockJobId::ExecuteScheduledOrders => Cadence::Minutes(5),
        }
    }

    /// Run this job once against `db`, returning the number of rows it acted on.
    pub async fn run(&self, db: &Db) -> Result<u64, JobError> {
        match self {
            ClockJobId::ActivateSubscriptions => subscriptions::activate_subscriptions(db).await,
            ClockJobId::TerminateEndedSubscriptions => {
                subscriptions::terminate_ended_subscriptions(db).await
            }
            ClockJobId::ExpireIncompleteSubscriptions => {
                subscriptions::expire_incomplete_subscriptions(db).await
            }
            ClockJobId::FreeTrialSubscriptionsBiller => subscriptions::free_trial_biller(db).await,
            ClockJobId::SubscriptionsToBeTerminated => {
                subscriptions::subscriptions_to_be_terminated(db).await
            }
            ClockJobId::FinalizeInvoices => invoices::finalize_invoices(db).await,
            ClockJobId::RefreshDraftInvoices => invoices::refresh_draft_invoices(db).await,
            ClockJobId::MarkInvoicesAsPaymentOverdue => {
                invoices::mark_invoices_as_payment_overdue(db).await
            }
            ClockJobId::RetryFailedInvoices => invoices::retry_failed_invoices(db).await,
            ClockJobId::RetryGeneratingSubscriptionInvoices => {
                invoices::retry_generating_subscription_invoices(db).await
            }
            ClockJobId::TerminateCoupons => coupons::terminate_coupons(db).await,
            ClockJobId::TerminateWallets => wallets::terminate_wallets(db).await,
            ClockJobId::TerminateRecurringTransactionRules => {
                wallets::terminate_recurring_transaction_rules(db).await
            }
            ClockJobId::CreateIntervalWalletTransactions => {
                wallets::create_interval_wallet_transactions(db).await
            }
            ClockJobId::RefreshWalletsOngoingBalance => {
                wallets::refresh_wallets_ongoing_balance(db).await
            }
            ClockJobId::RefreshLifetimeUsages => {
                // `LifetimeUsages::RecalculateAndCheckJob`: refresh, then check
                // usage thresholds (progressive billing).
                let refreshed = usage::refresh_lifetime_usages(db).await?;
                let issued = crate::progressive_billing::run_usage_threshold_checks(db).await?;
                Ok(refreshed + issued)
            }
            ClockJobId::ComputeAllDailyUsages => usage::compute_all_daily_usages(db).await,
            ClockJobId::WebhooksCleanup => webhooks::webhooks_cleanup(db).await,
            ClockJobId::InboundWebhooksCleanup => webhooks::inbound_webhooks_cleanup(db).await,
            ClockJobId::InboundWebhooksRetry => webhooks::inbound_webhooks_retry(db).await,
            ClockJobId::ExpireOrderForms => orders::expire_order_forms(db).await,
            ClockJobId::ExecuteScheduledOrders => orders::execute_scheduled_orders(db).await,
        }
    }
}

/// One job's outcome in a tick.
#[derive(Debug, Clone)]
pub struct JobOutcome {
    pub name: &'static str,
    pub affected: u64,
}

/// What a scheduler tick did.
#[derive(Debug, Default)]
pub struct ClockReport {
    pub ran: Vec<JobOutcome>,
}

impl ClockReport {
    /// Total rows transitioned across all jobs in the tick.
    pub fn total_affected(&self) -> u64 {
        self.ran.iter().map(|o| o.affected).sum()
    }

    /// Affected count for a named job, if it ran.
    pub fn affected(&self, name: &str) -> Option<u64> {
        self.ran.iter().find(|o| o.name == name).map(|o| o.affected)
    }
}

/// Run every registered clock job once (a full sweep). Errors are propagated,
/// so a failing job stops the tick — the caller (bin loop) logs and retries next
/// tick.
pub async fn run_all(db: &Db) -> Result<ClockReport, JobError> {
    let mut report = ClockReport::default();
    for job in ALL_JOBS {
        let affected = job.run(db).await?;
        report.ran.push(JobOutcome { name: job.name(), affected });
    }
    Ok(report)
}

/// Run only the jobs whose cadence elapsed in `(prev, now]`.
pub async fn run_due(
    db: &Db,
    prev: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<ClockReport, JobError> {
    let mut report = ClockReport::default();
    for job in ALL_JOBS {
        if job.cadence().is_due(prev, now) {
            // One failing job must not starve the others of their slot: log it
            // and continue (it runs again at its next cadence).
            match job.run(db).await {
                Ok(affected) => report.ran.push(JobOutcome { name: job.name(), affected }),
                Err(e) => tracing::error!(job = job.name(), error = %e, "clock job failed"),
            }
        }
    }
    Ok(report)
}

/// The full schedule as `(job_name, cron)` pairs (for the bin banner / docs).
pub fn schedule() -> Vec<(&'static str, String)> {
    ALL_JOBS.iter().map(|j| (j.name(), j.cadence().cron())).collect()
}

#[cfg(test)]
mod cadence_tests {
    use super::*;

    fn t(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 6, 15, h, m, 0).unwrap()
    }

    #[test]
    fn minutes_cadence_fires_once_per_bucket() {
        let c = Cadence::Minutes(5);
        // Crossing from 10:04 to 10:06 crosses the 10:05 boundary → due.
        assert!(c.is_due(t(10, 4), t(10, 6)));
        // Two ticks inside the same 5-min bucket → not due again.
        assert!(!c.is_due(t(10, 6), t(10, 8)));
    }

    #[test]
    fn hourly_at_fires_when_crossing_the_minute() {
        let c = Cadence::HourlyAt(25);
        assert!(c.is_due(t(10, 20), t(10, 30)));
        assert!(!c.is_due(t(10, 30), t(10, 40)));
        // Next hour fires again.
        assert!(c.is_due(t(10, 40), t(11, 26)));
    }

    #[test]
    fn daily_at_fires_once_per_day() {
        let c = Cadence::DailyAt(1, 0);
        let before = Utc.with_ymd_and_hms(2026, 6, 15, 0, 30, 0).unwrap();
        let after = Utc.with_ymd_and_hms(2026, 6, 15, 1, 30, 0).unwrap();
        assert!(c.is_due(before, after));
        let later = Utc.with_ymd_and_hms(2026, 6, 15, 2, 0, 0).unwrap();
        assert!(!c.is_due(after, later));
    }
}
