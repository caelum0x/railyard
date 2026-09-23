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

    if !rago_integrations::LIVE {
        tracing::warn!(
            "rago-server built without `--features live`: payment collection, provider \
             integrations and outbound webhook delivery are disabled (calls fail and are recorded as such)"
        );
    }

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
        let transport = rago_integrations::default_transport();
        let mut clock_prev = chrono::Utc::now();
        loop {
            let now = chrono::Utc::now();
            if let Err(e) = tick(&worker_db, &transport, billing_hour, clock_prev, now).await {
                tracing::error!(error = %e, "daemon tick failed");
            }
            clock_prev = now;
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

/// One daemon tick: subscription crons, the clock registry (finalize invoices,
/// wallet top-ups / expiry, coupon expiry, usage snapshots, ...), dunning, the
/// once-a-day billing cycle (claimed in Postgres so it is enqueued exactly once
/// per day across replicas), the durable job queue (billing → invoice →
/// webhooks → payment collection), and outbound webhook delivery.
async fn tick<T: rago_integrations::HttpTransport>(
    db: &Db,
    transport: &T,
    billing_hour: u32,
    clock_prev: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), Box<dyn std::error::Error>> {
    use chrono::Timelike;

    let crons = rago_jobs::run_subscription_crons(db).await?;
    let clock = rago_jobs::run_due_clocks(db, clock_prev, now).await?;
    let dunning_cfg = rago_jobs::DunningConfig::from_env();
    let dunning = rago_jobs::run_dunning(db, transport, &dunning_cfg).await?;

    // Enqueue the billing cycle once per day at (or after) the configured hour.
    let mut billing_enqueued = 0usize;
    if now.hour() >= billing_hour {
        let today = now.date_naive();
        if db.claim_daily_run("billing_cycle", today).await? {
            billing_enqueued = rago_jobs::enqueue_billing_cycle(db, today, false).await?.len();
        }
    }
    // Integrations: refresh expiring OAuth tokens and enqueue the accounting
    // auto-syncs (`sync_invoices` / `sync_credit_notes`) before draining.
    let (tokens_refreshed, syncs_enqueued) =
        rago_jobs::integrations_sync::run_integration_maintenance(db, transport).await;
    let jobs = rago_jobs::drain(db, "rago-server").await?;
    let hooks = rago_jobs::redeliver_due_webhooks(db, transport).await?;

    tracing::info!(
        activated = crons.activated,
        terminated = crons.terminated,
        clock_jobs = clock.ran.len(),
        dunned = dunning.invoices_marked_overdue,
        payment_requests = dunning.payment_requests_created,
        billing_enqueued,
        tokens_refreshed,
        syncs_enqueued,
        jobs,
        hooks,
        "daemon tick"
    );
    Ok(())
}
