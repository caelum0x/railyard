//! `rago-server` — the single supervising binary that boots the whole product:
//! the REST + GraphQL API *and* the background scheduler/worker daemon in one
//! process. This is the "run Rago as one thing" entrypoint.
//!
//! * Connects to Postgres (`DATABASE_URL`), applies embedded migrations.
//! * Spawns the axum API server (`RAGO_API_ADDR`, default `0.0.0.0:3000`).
//! * Spawns a background daemon loop (every `RAGO_TICK_SECS`, default 15s) that
//!   runs subscription crons, dunning, the durable job queue, the billing
//!   cycle, and webhook re-delivery — the same services the standalone worker
//!   uses.
//!
//! Built with `--features live`, the email transport becomes real SMTP and the
//! webhook re-delivery transport becomes real reqwest.

use std::time::Duration;

use rago_api::AppState;
use rago_db::Db;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Fail fast on a missing JWT secret (release builds refuse the insecure
    // development fallback) before accepting any traffic.
    rago_graphql::auth::validate_jwt_config()?;

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/rago".into());
    let db = Db::connect(&database_url).await?;
    db.migrate().await?;
    tracing::info!("connected to Postgres and applied migrations");

    // Build application state (live SMTP mailer when compiled with `--features live`).
    #[cfg(feature = "live")]
    let state = AppState::new_live(db.clone())?;
    #[cfg(not(feature = "live"))]
    let state = AppState::new(db.clone());

    // Background scheduler/worker daemon.
    let worker_db = db.clone();
    let daemon = tokio::spawn(async move {
        let interval: u64 = std::env::var("RAGO_TICK_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(15);
        let billing_hour: u32 = std::env::var("RAGO_BILLING_HOUR")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        loop {
            if let Err(e) = tick(&worker_db, billing_hour).await {
                tracing::error!(error = %e, "daemon tick failed");
            }
            tokio::time::sleep(Duration::from_secs(interval)).await;
        }
    });

    // API server.
    let addr = std::env::var("RAGO_API_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".into());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!(%addr, "rago-server listening (API + worker daemon)");
    let server = axum::serve(listener, rago_api::router(state));

    tokio::select! {
        r = server => { r?; }
        _ = daemon => { tracing::warn!("daemon task ended"); }
    }
    Ok(())
}

/// One daemon tick: crons + dunning + drain job queue + billing cycle +
/// webhook re-delivery.
async fn tick(db: &Db, billing_hour: u32) -> Result<(), Box<dyn std::error::Error>> {
    use chrono::{Timelike, Utc};

    let crons = rago_jobs::run_subscription_crons(db).await?;
    let dunning_transport = webhook_transport();
    let dunning_cfg = rago_jobs::DunningConfig::from_env();
    let dunning = rago_jobs::run_dunning(db, &dunning_transport, &dunning_cfg).await?;
    let dunned = dunning.invoices_marked_overdue;
    let jobs = rago_jobs::drain(db, "rago-server").await?;

    // Enqueue + run the billing cycle once at the configured hour.
    if Utc::now().hour() == billing_hour {
        let today = Utc::now().date_naive();
        let _ = rago_jobs::enqueue_billing_cycle(db, today, false).await?;
        let _ = rago_jobs::drain(db, "rago-server").await?;
    }

    let transport = webhook_transport();
    let hooks = rago_jobs::redeliver_due_webhooks(db, &transport).await?;

    tracing::info!(
        activated = crons.activated,
        terminated = crons.terminated,
        dunned,
        jobs,
        hooks,
        "daemon tick"
    );
    Ok(())
}

/// The outbound-webhook re-delivery transport: real reqwest under `--features
/// live`, otherwise an in-memory transport (no network).
#[cfg(feature = "live")]
fn webhook_transport() -> rago_integrations::ReqwestTransport {
    rago_integrations::ReqwestTransport::default()
}
#[cfg(not(feature = "live"))]
fn webhook_transport() -> rago_integrations::MockTransport {
    rago_integrations::MockTransport::always(200, "{}")
}
