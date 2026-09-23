//! `game-economy-server` — the runnable storefront.
//!
//! This binary is what makes the game-economy product actually drivable: it puts
//! the [`GameStore`](game_economy_core::GameStore) purchase state machine behind
//! an HTTP surface, wiring the REAL [`HttpMintTransport`] to a live mint-server
//! and the restart-safe [`JsonFileGameState`] for local durability. Every
//! purchase is a success-only hold → capture → receipt through Mint; the store
//! never reimplements the ledger. The router (routes, auth, handlers) lives in the
//! crate's library (`lib.rs`) so it is testable with an injected FakeMint.
//!
//! Routes:
//! - `GET  /healthz`                 — liveness (public).
//! - `GET  /v1/catalog`              — list SKUs + µ$ prices (public).
//! - `POST /v1/purchase`             — buy a SKU (bearer-auth; charges only the authenticated principal).
//! - `POST /v1/packs/open`           — open an owned pack (bearer-auth).
//! - `GET  /v1/inventory/:player`    — a player's granted entitlements (bearer-auth).
//! - `GET  /v1/purchases/:player[/:receipt_id]` — receipt history / lookup (bearer-auth).
//! - `GET  /store`                   — player storefront page.
//! - `POST /v1/pass/advance`         — report earned battle-pass tier (game-server key).
//! - `POST /v1/tokens`               — issue an expiring player token (game-server key).
//! - `POST /v1/entitlements/revoke`  — chargeback/refund revocation (game-server key).
//!
//! Configuration (fail-fast; nothing hardcoded):
//! - `RAILYARD_MINT_URL`       — base URL of the live mint-server (required).
//! - `MINT_API_KEY`            — Mint `x-api-key` secret (required).
//! - `RAILYARD_MINT_TIMEOUT_MS`— per-request Mint timeout (optional).
//! - `GAME_REVENUE_ACCOUNT`    — Mint account/address captures settle to (required).
//! - `GAME_AUTH_SECRET`        — shared secret used to verify per-player bearer tokens on the money-path routes (required; no unauthenticated deployments).
//! - `GAME_SERVER_SECRET`      — game-backend credential (`X-Game-Server-Key`) for the server-to-server routes (optional; those routes answer 503 until set).
//! - `GAME_ALLOW_LEGACY_TOKENS`— `true` to keep accepting non-expiring legacy tokens (default false).
//! - `GAME_TOKEN_TTL_SECS`     — default lifetime of issued tokens (default 86400).
//! - `GAME_CATALOG_PATH`       — JSON catalog definition (SKUs, prices, drop tables, seasons). Required unless `GAME_CATALOG=demo`.
//! - `GAME_FULFILLMENT_URL`    — game-backend delivery webhook; when set, capture happens only after it returns 2xx.
//! - `GAME_FULFILLMENT_SECRET` — MAC key for the delivery webhook (required with the URL).
//! - `GAME_FULFILLMENT_TIMEOUT_MS` — delivery webhook timeout (default 5000).
//! - `GAME_STATE_PATH`         — JSON durability file (default `./game-economy-state.json`).
//! - `GAME_BIND_ADDR`          — listen address (default `0.0.0.0:8102`).

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;

use game_economy_core::{Catalog, GameStore};
use game_economy_server::fulfill::{ServerFulfiller, WebhookFulfiller};
use game_economy_mint_client::HttpMintTransport;
use game_economy_server::{auth, build_router, AppState};
use game_economy_store::JsonFileGameState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let revenue_account = required_env("GAME_REVENUE_ACCOUNT")?;
    // Auth is mandatory: refuse to boot an unauthenticated storefront (the money
    // path would otherwise charge any account named in the request body).
    let auth_secret = required_env("GAME_AUTH_SECRET")?;
    let auth_key = auth::derive_key(auth_secret.as_bytes());
    let state_path = std::env::var("GAME_STATE_PATH")
        .unwrap_or_else(|_| "./game-economy-state.json".to_string());
    let bind: SocketAddr = std::env::var("GAME_BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8102".to_string())
        .parse()
        .context("GAME_BIND_ADDR must be a valid socket address")?;

    // Real Mint client from the environment (RAILYARD_MINT_URL + MINT_API_KEY).
    let mint = HttpMintTransport::from_env()
        .map_err(|e| anyhow::anyhow!("configuring Mint transport: {e}"))?;

    // Restart-safe local durability for the single-process storefront.
    let game_state = JsonFileGameState::open(&state_path)
        .map_err(|e| anyhow::anyhow!("opening game state {state_path}: {e}"))?;

    let catalog = load_catalog()?;
    let fulfiller = load_fulfiller()?;
    let store = GameStore::with_state(
        mint,
        fulfiller,
        catalog,
        revenue_account.clone(),
        Arc::new(game_state),
    );

    let mut state = AppState::new(Arc::new(store), auth_key)
        .with_legacy_tokens(env_flag("GAME_ALLOW_LEGACY_TOKENS"));
    if let Ok(ttl) = std::env::var("GAME_TOKEN_TTL_SECS") {
        let secs: u64 = ttl
            .trim()
            .parse()
            .context("GAME_TOKEN_TTL_SECS must be a positive integer")?;
        state = state.with_token_ttl(secs);
    }
    match std::env::var("GAME_SERVER_SECRET") {
        Ok(secret) if secret.trim().len() >= 16 => {
            state = state.with_server_secret(secret.trim().as_bytes());
        }
        Ok(_) => anyhow::bail!("GAME_SERVER_SECRET must be at least 16 characters"),
        Err(_) => tracing::warn!(
            "GAME_SERVER_SECRET unset: /v1/pass/advance, /v1/tokens and /v1/entitlements/revoke are disabled (503)"
        ),
    }
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(%bind, revenue_account, state_path, "game-economy-server listening");
    axum::serve(listener, app)
        .await
        .context("axum serve failed")?;
    Ok(())
}

/// Load the operator's catalog. A real deployment points `GAME_CATALOG_PATH`
/// at its JSON definition; the built-in sample catalog is only used when
/// explicitly requested with `GAME_CATALOG=demo`.
fn load_catalog() -> anyhow::Result<Catalog> {
    if let Ok(path) = std::env::var("GAME_CATALOG_PATH") {
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading GAME_CATALOG_PATH {path}"))?;
        let catalog = Catalog::from_json(&raw).map_err(|e| anyhow::anyhow!("{path}: {e}"))?;
        tracing::info!(path, products = catalog.len(), "catalog loaded");
        return Ok(catalog);
    }
    if std::env::var("GAME_CATALOG").as_deref() == Ok("demo") {
        tracing::warn!("GAME_CATALOG=demo: serving the built-in sample catalog");
        return Ok(Catalog::demo());
    }
    anyhow::bail!("GAME_CATALOG_PATH must point at a catalog JSON file (or set GAME_CATALOG=demo)")
}

/// Real delivery through the game backend when `GAME_FULFILLMENT_URL` is set;
/// otherwise the storefront inventory is the delivery record.
fn load_fulfiller() -> anyhow::Result<ServerFulfiller> {
    let Ok(url) = std::env::var("GAME_FULFILLMENT_URL") else {
        tracing::warn!(
            "GAME_FULFILLMENT_URL unset: goods are delivered to the storefront inventory only"
        );
        return Ok(ServerFulfiller::InventoryOnly);
    };
    let secret = required_env("GAME_FULFILLMENT_SECRET")?;
    let timeout_ms: u64 = std::env::var("GAME_FULFILLMENT_TIMEOUT_MS")
        .ok()
        .map(|v| v.trim().parse())
        .transpose()
        .context("GAME_FULFILLMENT_TIMEOUT_MS must be an integer")?
        .unwrap_or(5_000);
    let webhook = WebhookFulfiller::new(
        url.trim().to_string(),
        secret.as_bytes(),
        std::time::Duration::from_millis(timeout_ms),
    )
    .map_err(|e| anyhow::anyhow!(e))?;
    Ok(ServerFulfiller::Webhook(webhook))
}

fn env_flag(key: &str) -> bool {
    matches!(
        std::env::var(key).map(|v| v.trim().to_ascii_lowercase()).as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

fn required_env(key: &str) -> anyhow::Result<String> {
    std::env::var(key)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .with_context(|| format!("{key} must be set"))
}

fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .init();
}
