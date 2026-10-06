//! # rithub-server
//!
//! The axum HTTP service that makes rithub a real, self-hostable git forge:
//!
//! - a **forge JSON API** (`/api/...`) for organizations and repositories,
//!   backed by Postgres via `rithub-core`;
//! - a **dumb-HTTP git server** (`/{owner}/{repo}.git/...`) that hosts the
//!   `rit`-backed bare repositories, so a stock `git clone`/`git fetch` works;
//! - a **durable job worker** (tokio task) consuming `outbox_jobs` with
//!   retry + backoff for webhooks, notifications, audit, and email.
//!
//! Configuration is via environment (fail-closed on missing required values):
//!
//! | Var | Meaning | Default |
//! |-----|---------|---------|
//! | `DATABASE_URL` | Postgres connection string | *required* |
//! | `RITHUB_DATA_ROOT` | directory that holds bare repos | `./rithub-data` |
//! | `RITHUB_BIND` | listen address | `127.0.0.1:8080` |
//! | `SMTP_HOST` | outbound SMTP hostname | *(email disabled if absent)* |
//! | `SMTP_PORT` | SMTP port | `587` |
//! | `SMTP_USER` | SMTP username | *(required when SMTP_HOST is set)* |
//! | `SMTP_PASS` | SMTP password | *(required when SMTP_HOST is set)* |
//! | `SMTP_FROM` | From address | `rithub@<SMTP_HOST>` |
//! | `RITHUB_ADMIN_USERS` | comma-separated site-admin usernames | *(none)* |
//! | `RITHUB_OPEN_SIGNUP` | allow self-service sign-up (`false` = admins create accounts) | `true` |
//! | `RITHUB_PUBLIC_URL` | external base URL (clone URLs, secure cookies) | *(from request)* |
//! | `RITHUB_SSH_PUBLIC` | SSH clone host[:port] shown in the UI | *(from `RITHUB_SSH_BIND`)* |
//! | `RITHUB_WEBHOOK_ALLOW_PRIVATE` | allow webhooks to private/loopback hosts | `false` |
//! | `RAGO_BASE_URL` / `RAGO_API_KEY` | Rago plan enforcement + usage metering | *(billing off)* |
//! | `RITHUB_FREE_PRIVATE_REPOS` | private-repo cap when Rago is not configured | *(unlimited)* |

use std::path::PathBuf;

use anyhow::Context;
use rithub_core::PgStore;
use rithub_git::RepoHost;
use rithub_server::{build_router, jobs, search_index, ssh};
use rithub_server::state::{AppState, SmtpConfig};
use sqlx::postgres::PgPoolOptions;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let database_url = std::env::var("DATABASE_URL")
        .context("DATABASE_URL must be set (Postgres connection string)")?;
    let data_root: PathBuf = std::env::var("RITHUB_DATA_ROOT")
        .unwrap_or_else(|_| "./rithub-data".to_string())
        .into();
    let bind = std::env::var("RITHUB_BIND").unwrap_or_else(|_| "127.0.0.1:8080".to_string());

    std::fs::create_dir_all(&data_root)
        .with_context(|| format!("creating data root {}", data_root.display()))?;

    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&database_url)
        .await
        .context("connecting to Postgres")?;
    sqlx::migrate!("../../migrations")
        .run(&pool)
        .await
        .context("running database migrations")?;

    // Optional SMTP: configured when SMTP_HOST is present. Misconfigured SMTP
    // (bad creds, unreachable host) is non-fatal at startup — email jobs will
    // fail and retry until the config is corrected.
    let smtp_config = SmtpConfig::from_env();
    let mailer = smtp_config
        .as_ref()
        .and_then(|c| {
            c.build_transport()
                .inspect_err(|e| {
                    tracing::warn!(error = %e, "SMTP transport build failed — email disabled");
                })
                .ok()
        });
    if smtp_config.is_some() && mailer.is_some() {
        tracing::info!(host = %smtp_config.as_ref().unwrap().host, "SMTP transport ready");
    } else if smtp_config.is_some() {
        tracing::warn!("SMTP_HOST is set but transport build failed — check SMTP_* vars");
    } else {
        tracing::info!("SMTP not configured (SMTP_HOST unset) — email notifications disabled");
    }

    let state = AppState::new(
        PgStore::new(pool),
        RepoHost::new(&data_root),
        mailer,
        data_root.clone(),
    );

    // Full-text search: open the on-disk Tantivy index (default backend) and
    // attach it. Under `RITHUB_SEARCH_BACKEND=postgres` the native index is
    // skipped and the tsvector `search_documents` table answers queries.
    let state = if search_index::SearchBackend::from_env()
        == search_index::SearchBackend::Tantivy
    {
        let index_dir = data_root.join("search-index");
        match forge_search::SearchIndex::open(&index_dir) {
            Ok(idx) => {
                tracing::info!(dir = %index_dir.display(), "search index (tantivy) ready");
                state.with_search(idx)
            }
            Err(error) => {
                tracing::error!(%error, "opening search index failed — search degraded");
                state
            }
        }
    } else {
        tracing::info!("search backend = postgres (tsvector); native index disabled");
        state
    };

    // One-shot maintenance subcommand: `rithub-server reindex` rebuilds the
    // search index from Postgres + git, then exits.
    if std::env::args().nth(1).as_deref() == Some("reindex") {
        let n = search_index::reindex_all(&state).await?;
        tracing::info!(documents = n, "reindex complete");
        return Ok(());
    }

    // SSH transport: enabled when RITHUB_SSH_BIND is set.
    // The host key is persisted at $RITHUB_DATA_ROOT/ssh_host_key (ed25519,
    // generated on first boot) so clients can pin the server's fingerprint.
    if let Ok(ssh_bind) = std::env::var("RITHUB_SSH_BIND") {
        let rit_bin = std::env::var("RITHUB_RIT_BIN").unwrap_or_else(|_| "rit".to_string());
        let ssh_state = state.clone();
        let host_key_path = data_root.join("ssh_host_key");
        tokio::spawn(async move {
            if let Err(error) = ssh::serve(ssh_state, ssh_bind, rit_bin, &host_key_path).await {
                tracing::error!(%error, "SSH transport stopped");
            }
        });
    }

    // Durable job worker.
    {
        let worker_state = state.clone();
        tokio::spawn(async move {
            jobs::run_worker(worker_state).await;
        });
    }

    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(%bind, data_root = %data_root.display(), "rithub-server listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("serving")?;
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,rithub_server=debug"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .init();
}

async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("shutdown signal received");
}
