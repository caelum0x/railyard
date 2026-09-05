//! Rago billing scheduler — runs billing cycles that auto-bill subscriptions at
//! their period boundary, then charge + webhook.
//!
//! Env:
//! * `DATABASE_URL`      — Postgres.
//! * `RAGO_BILLING_DATE` — `YYYY-MM-DD` cycle date (default: first of next month).
//! * `RAGO_FORCE_BILL=1` — bill all active subscriptions regardless of due date.
//! * `RAGO_STRIPE_SECRET`— provider key (owner-gated; the live network call is
//!   behind the transport's `live` feature — this build uses the in-memory
//!   transport, which returns a scripted `succeeded` PaymentIntent).
//! * `RAGO_ONCE=1`       — run a single cycle and exit (default loops daily).

use std::str::FromStr;

use chrono::NaiveDate;
use rago_db::Db;
use rago_integrations::MockTransport;
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
        stripe_secret_key: std::env::var("RAGO_STRIPE_SECRET")
            .unwrap_or_else(|_| "sk_test_mock".into()),
        default_webhook_endpoint: Some(
            std::env::var("RAGO_WEBHOOK_URL")
                .unwrap_or_else(|_| "https://example.test/hooks".into()),
        ),
        charge_customers: true,
        provider_customer_id: std::env::var("RAGO_PROVIDER_CUSTOMER")
            .unwrap_or_else(|_| "cus_demo".into()),
    };

    // In-memory transport that both Stripe and the webhook deliverer talk to.
    // A live reqwest transport (feature `live`, owner-gated) would replace this.
    let transport = MockTransport::always(200, r#"{"id":"pi_mock_1","status":"succeeded"}"#);

    let run_once = std::env::var("RAGO_ONCE").as_deref() == Ok("1");
    loop {
        tracing::info!(%billing_date, force, "running billing cycle");
        let report = run_billing_cycle(&db, &transport, &cfg, billing_date, force).await?;
        tracing::info!(
            invoices = report.billed_invoices.len(),
            payments_succeeded = report.payments_succeeded,
            payments_failed = report.payments_failed,
            webhooks = report.webhooks_written,
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
