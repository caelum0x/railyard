//! Production Ledgerline server wiring.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{bail, Context};
use ledgerline::mint::{MintClient, MintSettlement};
use ledgerline::outbox::{OutboxStore, PostgresOutbox};
use ledgerline::policy::AccountPolicy;
use ledgerline::recovery::{spawn_recovery_worker, RecoveryConfig};
use ledgerline::server::{build_router, AppState};
use ledgerline::settle::{Clock, SettlementCoordinator, SystemClock};
use ledgerline::store::{EventStore, PostgresStore};

struct Config {
    bind_addr: SocketAddr,
    database_url: String,
    mint_base: String,
    mint_api_key: String,
    ledgerline_api_key: String,
    policy: AccountPolicy,
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
            policy: load_policy()?,
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let config = Config::from_env()?;

    let store = Arc::new(
        PostgresStore::connect(&config.database_url)
            .await
            .context("connecting to Ledgerline Postgres")?,
    );
    store
        .migrate()
        .await
        .context("running Ledgerline migrations")?;

    // The coordinator drives the hold+capture+COGS trio through a durable outbox
    // so a mid-sequence failure is atomic-or-compensated, never half-settled.
    let mint_ops = Arc::new(MintClient::new(config.mint_base, config.mint_api_key));
    let outbox: Arc<dyn OutboxStore> = Arc::new(PostgresOutbox::new(store.pool().clone()));
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let coordinator = Arc::new(SettlementCoordinator::new(
        mint_ops,
        outbox.clone(),
        clock.clone(),
    ));

    // Autonomous recovery: on a fixed cadence, finish (or compensate) any saga a
    // crash left durably incomplete, and backfill the margin projection for any
    // settled-but-unprojected request — all without waiting for a new inbound
    // retry. Every money step is idempotency-keyed and the projection write is
    // idempotent, so this never double-charges or double-projects. The handle
    // lives for the process lifetime.
    let settler: Arc<dyn MintSettlement> = coordinator.clone();
    let event_store: Arc<dyn EventStore> = store.clone();
    let _recovery = spawn_recovery_worker(
        settler,
        event_store.clone(),
        outbox,
        clock,
        RecoveryConfig::default(),
    );

    let state = AppState {
        mint: coordinator,
        store: event_store,
        api_key: Arc::from(config.ledgerline_api_key),
        policy: Arc::new(config.policy),
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

/// Load the Mint account policy. Ledgerline moves money between the accounts an
/// event names, so running without a policy is refused unless explicitly opted
/// into for local development.
fn load_policy() -> anyhow::Result<AccountPolicy> {
    match std::env::var("LEDGERLINE_ACCOUNT_POLICY_JSON") {
        Ok(raw) if !raw.trim().is_empty() => {
            AccountPolicy::from_json(&raw).map_err(anyhow::Error::msg)
        }
        _ => {
            if std::env::var("LEDGERLINE_ALLOW_UNRESTRICTED_ACCOUNTS").as_deref() == Ok("1") {
                tracing::warn!(
                    "LEDGERLINE_ALLOW_UNRESTRICTED_ACCOUNTS=1: events may name ANY Mint account \
                     (development only)"
                );
                Ok(AccountPolicy::unrestricted())
            } else {
                bail!(
                    "LEDGERLINE_ACCOUNT_POLICY_JSON must be set (allowlisted revenue/cogs/provider \
                     accounts + customer_id -> account mapping); set \
                     LEDGERLINE_ALLOW_UNRESTRICTED_ACCOUNTS=1 only for local development"
                )
            }
        }
    }
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
