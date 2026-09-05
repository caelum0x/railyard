//! Rago clock — the billing-automation scheduler.
//!
//! Ports Lago's `clock` process (`config/clock.rb`): on every minute-tick it
//! runs the `Clock::*` jobs whose cadence elapsed since the previous tick
//! (subscription activation/termination, invoice finalization/overdue,
//! wallet/coupon lifecycle, usage refresh, webhook housekeeping, …).
//!
//! Env:
//! * `DATABASE_URL` — Postgres.
//! * `RAGO_ONCE=1`  — run every job once and exit (default: loop each minute).
//!
//! Subscription auto-billing (`Clock::SubscriptionsBillerJob`) lives in the
//! sibling `rago-scheduler` bin, which owns the payment-provider transport.

use chrono::Utc;
use rago_db::Db;
use rago_jobs::clock;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/rago".into());
    let db = Db::connect(&database_url).await?;
    db.migrate().await?;

    for (name, cron) in clock::schedule() {
        tracing::info!(job = name, %cron, "registered clock job");
    }

    if std::env::var("RAGO_ONCE").as_deref() == Ok("1") {
        let report = clock::run_all(&db).await?;
        for outcome in &report.ran {
            println!("{} affected={}", outcome.name, outcome.affected);
        }
        return Ok(());
    }

    // Tick loop: every minute, run the jobs whose cadence elapsed.
    let mut prev = Utc::now();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        let now = Utc::now();
        match clock::run_due(&db, prev, now).await {
            Ok(report) => {
                for outcome in &report.ran {
                    tracing::info!(job = outcome.name, affected = outcome.affected, "clock job ran");
                }
            }
            Err(e) => tracing::error!(error = %e, "clock tick failed"),
        }
        prev = now;
    }
}
