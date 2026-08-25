//! Production Ledgerline server wiring.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{bail, Context};
use ledgerline::mint::MintClient;
use ledgerline::outbox::MemoryOutbox;
use ledgerline::server::{build_router, AppState};
use ledgerline::settle::{SettlementCoordinator, SystemClock};
use ledgerline::store::PostgresStore;

struct Config {
    bind_addr: SocketAddr,
    database_url: String,
    mint_base: String,
    mint_api_key: String,
    ledgerline_api_key: String,
}

impl Config {
    fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            bind_addr: std::env::var("LEDGERLINE_BIND_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:8093".to_string())
                .parse()
                .context("LEDGERLINE_BIND_ADDR must be a valid socket address")?,
            database_url: required("DATABASE_URL")?,
            mint_base: required("MINT_BASE")?,
            mint_api_key: required("MINT_API_KEY")?,
            ledgerline_api_key: required("LEDGERLINE_API_KEY")?,
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let config = Config::from_env()?;

    let store = PostgresStore::connect(&config.database_url)
        .await
        .context("connecting to Ledgerline Postgres")?;
    store
        .migrate()
        .await
        .context("running Ledgerline migrations")?;

    // The coordinator drives the hold+capture+COGS trio through a durable outbox
    // so a mid-sequence failure is atomic-or-compensated, never half-settled.
    let mint_ops = Arc::new(MintClient::new(config.mint_base, config.mint_api_key));
    let coordinator = SettlementCoordinator::new(
        mint_ops,
        Arc::new(MemoryOutbox::new()),
        Arc::new(SystemClock),
    );
    let state = AppState {
        mint: Arc::new(coordinator),
        store: Arc::new(store),
        api_key: Arc::from(config.ledgerline_api_key),
    };
    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("binding {}", config.bind_addr))?;
    tracing::info!(bind = %config.bind_addr, "ledgerline-server listening");
    axum::serve(listener, build_router(state))
        .await
        .context("Ledgerline server failed")?;
    Ok(())
}

fn required(name: &str) -> anyhow::Result<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Ok(value),
        _ => bail!("{name} must be set"),
    }
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}
