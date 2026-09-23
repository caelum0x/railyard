//! Rago billing scheduler — runs billing cycles that auto-bill subscriptions at
//! their period boundary, then charge + webhook.
//!
//! Env:
//! * `DATABASE_URL`      — Postgres.
//! * `RAGO_BILLING_DATE` — `YYYY-MM-DD` cycle date (default: first of next month).
//! * `RAGO_FORCE_BILL=1` — bill all active subscriptions regardless of due date.
//! * `RAGO_CHARGE=0`     — invoice only, do not collect payment.
//! * `RAGO_ONCE=1`       — run a single cycle and exit (default loops daily).
//!
//! Payment providers and credentials are resolved per organization (dashboard
//! `payment_providers`, else `RAGO_STRIPE_SECRET_KEY` & co.). Outbound calls
//! use the build's transport: real HTTP with `--features live`, otherwise a
//! disabled transport that fails loudly (nothing is marked paid or delivered).

use std::str::FromStr;

use chrono::NaiveDate;
use rago_db::Db;
use rago_jobs::scheduler::{first_of_next_month, run_billing_cycle, today, SchedulerConfig};

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

    let billing_date = std::env::var("RAGO_BILLING_DATE")
        .ok()
        .and_then(|s| NaiveDate::from_str(&s).ok())
        .unwrap_or_else(|| first_of_next_month(today()));
    let force = std::env::var("RAGO_FORCE_BILL").as_deref() == Ok("1");

    let cfg = SchedulerConfig {
        charge_customers: std::env::var("RAGO_CHARGE").as_deref() != Ok("0"),
    };
    let transport = rago_integrations::default_transport();
    if !rago_integrations::LIVE {
        tracing::warn!("built without `live`: provider charges and webhook deliveries will fail until rebuilt with --features live");
    }

    let run_once = std::env::var("RAGO_ONCE").as_deref() == Ok("1");
    loop {
        tracing::info!(%billing_date, force, "running billing cycle");
        let report = run_billing_cycle(&db, &transport, &cfg, billing_date, force).await?;
        // Deliver the webhooks the cycle queued.
        let delivered = rago_jobs::redeliver_due_webhooks(&db, &transport).await?;
        tracing::info!(
            invoices = report.billed_invoices.len(),
            payments_succeeded = report.payments_succeeded,
            payments_failed = report.payments_failed,
            payments_skipped = report.payments_skipped,
            errors = report.errors,
            webhooks = report.webhooks_written,
            delivered,
            "billing cycle complete"
        );
        println!(
            "CYCLE billed_invoices={} payments_succeeded={} payments_failed={} webhooks_written={}",
            report.billed_invoices.len(),
            report.payments_succeeded,
            report.payments_failed,
            report.webhooks_written
        );
        for id in &report.billed_invoices {
            println!("  invoice {id}");
        }

        if run_once {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(86_400)).await;
    }

    Ok(())
}
