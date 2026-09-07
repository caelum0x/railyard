//! `vend-server` binary: load config from the environment, wire the REAL
//! transports + Neon Postgres store, run migrations, and serve the data plane.
//!
//! Env:
//! - `VEND_BIND_ADDR`  — listen addr (default `0.0.0.0:8090`).
//! - `DATABASE_URL`    — Neon Postgres DSN (required).
//! - `MINT_BASE`       — mint-server base URL (required).
//! - `MINT_API_KEY`    — `X-API-Key` for mint-server (required).
//! - `RELAY_BASE`      — Relay base URL (required).
//! - `RELAY_KEY`       — upstream Relay bearer (required).
//! - `RELAY_BASE_PATH` — OpenAI-compatible surface path (default `/openai/v1`).
//! - `VEND_ADMIN_TOKEN_SHA256` — SHA-256 hex of the control bearer (required).
//! - `VEND_MODEL_RATES_JSON` — validated model rate catalog (required).
//! - `VEND_CLEARING_ACCOUNT` — Mint account receiving captures (required).
//! - `VEND_PROVIDER_PAYOUT_ACCOUNT` — provider's Mint destination (required).
//! - `VEND_PLATFORM_PAYOUT_ACCOUNT` — platform's Mint destination (required).

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{bail, Context};

use vend_mint_client::MintClient;
use vend_relay_client::RelayClient;
use vend_server::control::{
    build_control_router, AdminAuth, CustomerAuthConfig, OAuthProviderConfig,
};
use vend_server::pricing::PricingCatalog;
use vend_server::readiness::DependencyReadiness;
use vend_server::runtime_config::RuntimeConfig;
use vend_server::security_ops::SecurityOpsConfig;
use vend_server::{build_router, AppState, DataPlaneSecurity};
use vend_server::{observability, shutdown};
use vend_settlement::PayoutConfig;
use vend_store::VendStore;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let bind: SocketAddr = std::env::var("VEND_BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8090".to_string())
        .parse()
        .context("VEND_BIND_ADDR must be a valid socket address")?;
    let runtime = RuntimeConfig::from_env().map_err(anyhow::Error::msg)?;

    let database_url = require("DATABASE_URL")?;
    let mint_base = require("MINT_BASE")?;
    let mint_api_key = require("MINT_API_KEY")?;
    let relay_base = require("RELAY_BASE")?;
    let relay_key = require("RELAY_KEY")?;
    let payout_config = PayoutConfig::new(
        require("VEND_CLEARING_ACCOUNT")?,
        require("VEND_PROVIDER_PAYOUT_ACCOUNT")?,
        require("VEND_PLATFORM_PAYOUT_ACCOUNT")?,
    )
    .context("invalid Mint payout destinations")?;
    let settlement_account = payout_config.clearing_account_id.clone();
    let pricing = Arc::new(
        PricingCatalog::from_json(&require("VEND_MODEL_RATES_JSON")?)
            .context("invalid VEND_MODEL_RATES_JSON")?,
    );
    let admin_digest = require("VEND_ADMIN_TOKEN_SHA256")?;
    let admin_auth = AdminAuth::from_sha256_hexes(
        &admin_digest,
        optional("VEND_ADMIN_TOKEN_NEXT_SHA256").as_deref(),
    )
    .context("invalid VEND_ADMIN_TOKEN_SHA256")?;
    let security_config =
        SecurityOpsConfig::from_pepper_hex(&require("VEND_RATE_LIMIT_PEPPER_HEX")?)
            .context("invalid VEND_RATE_LIMIT_PEPPER_HEX")?;
    let customer_auth = CustomerAuthConfig::new(
        &require("VEND_PUBLIC_BASE_URL")?,
        oauth_provider("GITHUB")?,
        oauth_provider("GOOGLE")?,
        optional_positive_i64("VEND_CUSTOMER_SESSION_TTL_SECONDS")?.unwrap_or(86_400),
        optional_positive_i64("VEND_CUSTOMER_SETUP_TTL_SECONDS")?.unwrap_or(604_800),
    )
    .context("invalid customer authentication configuration")?;
    let relay_base_path =
        std::env::var("RELAY_BASE_PATH").unwrap_or_else(|_| "/openai/v1".to_string());

    // Neon Postgres store (runs migrations at startup).
    let store = VendStore::connect_with_config(&database_url, &runtime.postgres)
        .await
        .context("connecting to Postgres")?;
    store.migrate().await.context("running migrations")?;
    let store = Arc::new(store);

    // Fail closed unless Postgres, the authenticated Mint clearing account, and
    // Relay's dependency-aware health endpoint are all reachable.
    let readiness = Arc::new(
        DependencyReadiness::new(
            store.clone(),
            &mint_base,
            &mint_api_key,
            &settlement_account,
            &relay_base,
            optional("RELAY_HEALTH_URL").as_deref(),
            runtime.readiness_timeout,
        )
        .context("invalid dependency readiness configuration")?,
    );

    // Real HTTP transports with explicit connect, total, idle-read, and pool
    // bounds. Redirects are refused by both clients to contain credentials.
    let mint = Arc::new(
        MintClient::with_config(mint_base, mint_api_key, runtime.mint_http.clone())
            .context("invalid Mint HTTP configuration")?,
    );
    let relay = Arc::new(
        RelayClient::with_config(relay_base, runtime.relay_http.clone())
            .context("invalid Relay HTTP configuration")?,
    );
    let metrics = Arc::new(observability::Metrics::default());

    let state = AppState {
        metrics: metrics.clone(),
        mint: mint.clone(),
        relay,
        tenants: store.clone(),
        keys: store.clone(),
        usage: store.clone(),
        payouts: store.clone(),
        readiness,
        idempotency: store.clone(),
        pricing,
        relay_base_path,
        relay_key,
        settlement_account,
        payout_config,
        security_ops: Some(DataPlaneSecurity {
            repo: store.clone(),
            config: security_config.clone(),
        }),
    };

    let app = observability::harden_router(
        build_router(state).merge(build_control_router(
            store.clone(),
            store,
            admin_auth,
            customer_auth,
            security_config,
            mint,
        )),
        metrics,
        runtime.limits,
    );
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(%bind, "vend-server listening");
    shutdown::serve_until(
        listener,
        app,
        shutdown::shutdown_signal(),
        runtime.shutdown_grace,
    )
    .await
    .context("server error")?;
    Ok(())
}

fn require(name: &str) -> anyhow::Result<String> {
    match std::env::var(name) {
        Ok(v) if !v.is_empty() => Ok(v),
        _ => bail!("{name} must be set (no unconfigured deployments)"),
    }
}

fn optional(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn oauth_provider(name: &str) -> anyhow::Result<Option<OAuthProviderConfig>> {
    let client_id = optional(&format!("VEND_{name}_CLIENT_ID"));
    let client_secret = optional(&format!("VEND_{name}_CLIENT_SECRET"));
    match (client_id, client_secret) {
        (None, None) => Ok(None),
        (Some(client_id), Some(client_secret)) => Ok(Some(OAuthProviderConfig {
            client_id,
            client_secret,
        })),
        _ => bail!("VEND_{name}_CLIENT_ID and VEND_{name}_CLIENT_SECRET must be set together"),
    }
}

fn optional_positive_i64(name: &str) -> anyhow::Result<Option<i64>> {
    optional(name)
        .map(|value| {
            value
                .parse::<i64>()
                .with_context(|| format!("{name} must be an integer"))
                .and_then(|value| {
                    if value > 0 {
                        Ok(value)
                    } else {
                        bail!("{name} must be positive")
                    }
                })
        })
        .transpose()
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    if std::env::var("VEND_LOG_FORMAT").as_deref() == Ok("pretty") {
        fmt().with_env_filter(filter).init();
    } else {
        fmt()
            .json()
            .flatten_event(true)
            .with_current_span(false)
            .with_span_list(false)
            .with_env_filter(filter)
            .init();
    }
}
