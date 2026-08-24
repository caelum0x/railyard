//! Production Character Lock Passport server wiring. Fail-closed on missing
//! config: no deployment silently starts without its database or credentials.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{bail, Context};
use context_passport::server::{build_router, AppState};
use context_passport::service::PassportService;
use context_passport::store::PostgresStore;

struct Config {
    bind_addr: SocketAddr,
    database_url: String,
    api_key: String,
}

impl Config {
    fn from_env() -> anyhow::Result<Self> {
        Ok(Self {
            bind_addr: std::env::var("PASSPORT_BIND_ADDR")
                .unwrap_or_else(|_| "0.0.0.0:8095".to_string())
                .parse()
                .context("PASSPORT_BIND_ADDR must be a valid socket address")?,
            database_url: required("DATABASE_URL")?,
            api_key: required("PASSPORT_API_KEY")?,
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let config = Config::from_env()?;

    let store = PostgresStore::connect(&config.database_url)
        .await
        .context("connecting to Passport Postgres")?;
    store
        .migrate()
        .await
        .context("running Passport migrations")?;

    let state = AppState {
        service: PassportService::new(Arc::new(store)),
        api_key: Arc::from(config.api_key),
    };
    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("binding {}", config.bind_addr))?;
    tracing::info!(bind = %config.bind_addr, "context-passport-server listening");
    axum::serve(listener, build_router(state))
        .await
        .context("Passport server failed")?;
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
