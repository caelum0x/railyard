//! Abacus proxy server entrypoint. Fail-fast config load, then wire the REAL
//! `abacus-mint-client` (live mint-server) + `abacus-upstream` (real provider),
//! plus a restart-safe local policy/receipt store, and serve.

use std::{net::SocketAddr, sync::Arc};

use anyhow::Context;

use abacus_core::hold_store::{Clock, HoldStore, SystemClock};
use abacus_core::identity::BudgetRepo;
use abacus_core::receipt::ReceiptStore;
use abacus_mint_client::MintClient;
use abacus_proxy::{
    build_router, hold_reaper::reap_expired_claimed, local_hold_store::LocalHoldStore,
    local_store::LocalStore, postgres_hold_store::PostgresHoldStore,
    postgres_product_store::PostgresProductStore, AppState, Config, PricingTable,
};
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

    let (budgets, receipts, holds): (
        Arc<dyn BudgetRepo>,
        Arc<dyn ReceiptStore>,
        Arc<dyn HoldStore>,
    ) = if let Some(database_url) = &config.database_url {
        let product = PostgresProductStore::connect(database_url)
            .await
            .context("connecting Abacus shared product store")?;
        product
            .migrate()
            .await
            .context("migrating Abacus shared product store")?;
        product
            .import_agent_config(std::path::Path::new(&config.agent_config_path))
            .await
            .context("installing Abacus policy projection")?;
        let holds = Arc::new(PostgresHoldStore::from_pool(product.pool()));
        let product = Arc::new(product);
        (product.clone(), product, holds)
    } else {
        tracing::warn!(
            "ABACUS_DATABASE_URL is unset; policy, receipts, and recovery are single-replica only"
        );
        let product = Arc::new(
            LocalStore::open(&config.agent_config_path, &config.receipt_log_path)
                .context("opening Abacus local product store")?,
        );
        let holds = Arc::new(
            LocalHoldStore::open(&config.hold_store_path)
                .context("opening Abacus durable hold store")?,
        );
        (product.clone(), product, holds)
    };
    let bind: SocketAddr = config
        .bind_addr
        .parse()
        .context("ABACUS_BIND_ADDR must be a valid socket address")?;
    let state = AppState {
        mint,
        upstream,
        budgets,
        receipts,
        holds: holds.clone(),
        ingress_api_key: config.abacus_api_key.clone(),
        pricing: Arc::new(PricingTable::builtin()),
        provider: config.provider.clone(),
        upstream_path: config.upstream_path.clone(),
    };

    // Sweep once before serving, then keep retrying transient Mint failures.
    let recovery_worker_id = format!("abacus_{}", ulid::Ulid::new());
    let recovery_lease_ms = config.hold_reaper_interval_ms.saturating_mul(4) as i64;
    match reap_expired_claimed(
        holds.as_ref(),
        state.mint.as_ref(),
        state.receipts.as_ref(),
        &SystemClock,
        config.hold_ttl_ms,
        &recovery_worker_id,
        SystemClock.now_ms().saturating_add(recovery_lease_ms),
        100,
    )
    .await
    {
        Ok(summary) => tracing::info!(
            reaped = summary.reaped_count(),
            released_micros = summary.released_micros,
            "startup orphan-hold sweep complete"
        ),
        Err(error) => {
            tracing::error!(%error, "startup orphan-hold sweep failed; periodic reaper will retry")
        }
    }
    let reaper_holds = holds;
    let reaper_mint = state.mint.clone();
    let reaper_receipts = state.receipts.clone();
    let reaper_ttl_ms = config.hold_ttl_ms;
    let reaper_interval = std::time::Duration::from_millis(config.hold_reaper_interval_ms);
    let reaper_worker_id = recovery_worker_id;
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(reaper_interval);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let now_ms = SystemClock.now_ms();
            match reap_expired_claimed(
                reaper_holds.as_ref(),
                reaper_mint.as_ref(),
                reaper_receipts.as_ref(),
                &SystemClock,
                reaper_ttl_ms,
                &reaper_worker_id,
                now_ms.saturating_add(recovery_lease_ms),
                100,
            )
            .await
            {
                Ok(summary) if summary.reaped_count() > 0 || summary.already_terminal > 0 => {
                    tracing::info!(
                        reaped = summary.reaped_count(),
                        already_terminal = summary.already_terminal,
                        released_micros = summary.released_micros,
                        "orphan-hold sweep complete"
                    )
                }
                Ok(_) => {}
                Err(error) => tracing::error!(%error, "orphan-hold sweep failed; will retry"),
            }
        }
    });

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(%bind, "abacus-proxy listening");
    axum::serve(listener, build_router(state))
        .await
        .context("server error")?;
    Ok(())
}
