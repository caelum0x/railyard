//! Abacus proxy server entrypoint. Fail-fast config load, then wire the REAL
//! `abacus-mint-client` (live mint-server) + `abacus-upstream` (real provider),
//! plus a restart-safe local policy/receipt store, and serve.

use std::{net::SocketAddr, sync::Arc};

use anyhow::Context;

use abacus_mint_client::MintClient;
use abacus_proxy::{build_router, local_store::LocalStore, AppState, Config, PricingTable};
use abacus_upstream::UpstreamClient;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    // Fail-fast config: every secret/URL comes from the environment, never a
    // literal. A missing var aborts startup before we bind.
    let config = Config::from_env().context("invalid Abacus configuration")?;

    // REAL transports against the live mint-server + provider.
    let mint = Arc::new(MintClient::new(
        config.mint_server_url.clone(),
        config.mint_api_key.clone(),
    ));
    let upstream = Arc::new(UpstreamClient::new(config.upstream_base_url.clone()));

    let store = Arc::new(
        LocalStore::open(&config.agent_config_path, &config.receipt_log_path)
            .context("opening Abacus product store")?,
    );
    let bind: SocketAddr = config
        .bind_addr
        .parse()
        .context("ABACUS_BIND_ADDR must be a valid socket address")?;
    let state = AppState {
        mint,
        upstream,
        budgets: store.clone(),
        receipts: store,
        ingress_api_key: config.abacus_api_key.clone(),
        pricing: Arc::new(PricingTable::builtin()),
        provider: config.provider.clone(),
        upstream_path: config.upstream_path.clone(),
    };

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(%bind, "abacus-proxy listening");
    axum::serve(listener, build_router(state))
        .await
        .context("server error")?;
    Ok(())
}
