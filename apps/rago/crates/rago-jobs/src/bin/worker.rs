//! Rago durable worker/CLI — drives the Postgres-backed job queue, crons, and
//! webhook re-delivery, and bootstraps tenants.
//!
//! Subcommands (argv, print the new API key once — see `rago_jobs::admin_cli`):
//! - `create-org --name <NAME> [--slug] [--currency] [--email] [--key-name]`
//! - `create-api-key --org <ORG_ID|SLUG> [--name]`
//!
//! Without a subcommand, the job is selected by
//! `RAGO_CMD`:
//! - `enqueue` — enqueue billing jobs for subs due on `RAGO_BILLING_DATE`.
//! - `enqueue-one` — enqueue one billing job (`RAGO_ORG`, `RAGO_SUB_EXTERNAL`,
//!   `RAGO_BILLING_DATE`, optional `RAGO_FAIL_UNTIL` to prove durable retries).
//! - `work` — lease + run all due jobs once (retries/backoff/dead-letter).
//! - `crons` — run the subscription-lifecycle crons once.
//! - `redeliver` — re-deliver due `retrying` webhooks (mock transport here; the
//!   live transport is owner-gated behind the `live` feature).

use std::str::FromStr;

use chrono::NaiveDate;
use rago_db::Db;
use serde_json::json;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let admin_cmd = match rago_jobs::admin_cli::parse_args(&argv) {
        Ok(cmd) => cmd,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };

    // Admin subcommands keep stdout for the `KEY=value` block only.
    let log_to_stderr = admin_cmd.is_some();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with_writer(move || -> Box<dyn std::io::Write> {
            if log_to_stderr { Box::new(std::io::stderr()) } else { Box::new(std::io::stdout()) }
        })
        .init();

    let db = Db::connect(
        &std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/rago".into()),
    )
    .await?;
    db.migrate().await?;

    if let Some(cmd) = admin_cmd {
        match rago_jobs::admin_cli::run(&db, &cmd).await {
            Ok(created) => {
                print!("{}", rago_jobs::admin_cli::render(&created));
                eprintln!("Store API_KEY in the consumer's secret store now; this command does not print it again.");
                return Ok(());
            }
            Err(e) => {
                eprintln!("error: {e}");
                std::process::exit(1);
            }
        }
    }

    let cmd = std::env::var("RAGO_CMD").unwrap_or_else(|_| "work".into());
    let billing_date = std::env::var("RAGO_BILLING_DATE")
        .ok()
        .and_then(|s| NaiveDate::from_str(&s).ok())
        .unwrap_or_else(|| chrono::Utc::now().date_naive());

    match cmd.as_str() {
        "enqueue" => {
            let ids = rago_jobs::enqueue_billing_cycle(&db, billing_date, false).await?;
            println!("ENQUEUED {} billing jobs", ids.len());
        }
        "enqueue-one" => {
            let org = Uuid::from_str(&std::env::var("RAGO_ORG")?)?;
            let ext = std::env::var("RAGO_SUB_EXTERNAL")?;
            let fail_until: i64 = std::env::var("RAGO_FAIL_UNTIL")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let payload = json!({
                "organization_id": org.to_string(),
                "subscription_external_id": ext,
                "billing_date": billing_date.format("%Y-%m-%d").to_string(),
                "simulate_failure_until_attempt": fail_until,
            });
            let id = db
                .enqueue_job(
                    rago_jobs::JOB_BILL_SUBSCRIPTION,
                    &payload,
                    chrono::Utc::now().naive_utc(),
                    5,
                )
                .await?;
            println!("ENQUEUED job {id} (fail_until={fail_until})");
        }
        "work" => {
            let n = rago_jobs::drain(&db, "worker-1").await?;
            println!("WORKED {n} jobs");
        }
        "crons" => {
            let r = rago_jobs::run_subscription_crons(&db).await?;
            println!(
                "CRONS activated={} terminated={} expired_incomplete={} free_trials_ended={}",
                r.activated, r.terminated, r.expired_incomplete, r.free_trials_ended
            );
        }
        "redeliver" => {
            let transport = rago_integrations::default_transport();
            let n = rago_jobs::redeliver_due_webhooks(&db, &transport).await?;
            println!("REDELIVERED {n} webhooks");
        }
        "dunning" => {
            let transport = rago_integrations::default_transport();
            let cfg = rago_jobs::DunningConfig::from_env();
            let r = rago_jobs::run_dunning(&db, &transport, &cfg).await?;
            println!(
                "DUNNING flagged_overdue={} evaluated={} requests={} charged={} finished={}",
                r.invoices_marked_overdue,
                r.customers_evaluated,
                r.payment_requests_created,
                r.invoices_charged,
                r.campaigns_finished
            );
        }
        "daemon" => {
            // Long-running worker: each tick runs crons + dunning, drains the
            // job queue, and re-delivers due webhooks.
            let interval = std::env::var("RAGO_TICK_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(5u64);
            let transport = rago_integrations::default_transport();
            let dunning_cfg = rago_jobs::DunningConfig::from_env();
            tracing::info!(interval, "worker daemon started");
            loop {
                let crons = rago_jobs::run_subscription_crons(&db).await?;
                let dunned = rago_jobs::run_dunning(&db, &transport, &dunning_cfg).await?;
                let (tokens_refreshed, syncs_enqueued) =
                    rago_jobs::integrations_sync::run_integration_maintenance(&db, &transport).await;
                let jobs = rago_jobs::drain(&db, "daemon").await?;
                let hooks = rago_jobs::redeliver_due_webhooks(&db, &transport).await?;
                tracing::info!(
                    activated = crons.activated,
                    terminated = crons.terminated,
                    dunned = dunned.invoices_marked_overdue,
                    requests = dunned.payment_requests_created,
                    tokens_refreshed,
                    syncs_enqueued,
                    jobs,
                    hooks,
                    "daemon tick"
                );
                tokio::time::sleep(std::time::Duration::from_secs(interval)).await;
            }
        }
        other => {
            eprintln!("unknown RAGO_CMD: {other}");
            std::process::exit(2);
        }
    }
    Ok(())
}
