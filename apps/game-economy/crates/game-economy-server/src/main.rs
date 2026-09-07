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
//!
//! Configuration (fail-fast; nothing hardcoded):
//! - `RAILYARD_MINT_URL`       — base URL of the live mint-server (required).
//! - `MINT_API_KEY`            — Mint `x-api-key` secret (required).
//! - `RAILYARD_MINT_TIMEOUT_MS`— per-request Mint timeout (optional).
//! - `GAME_REVENUE_ACCOUNT`    — Mint account/address captures settle to (required).
//! - `GAME_AUTH_SECRET`        — shared secret used to verify per-player bearer tokens on the money-path routes (required; no unauthenticated deployments).
//! - `GAME_STATE_PATH`         — JSON durability file (default `./game-economy-state.json`).
//! - `GAME_BIND_ADDR`          — listen address (default `0.0.0.0:8102`).

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;

use game_economy_core::{AlwaysFulfill, Catalog, GameStore};
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

    let catalog = Catalog::demo();
    let store = GameStore::with_state(
        mint,
        AlwaysFulfill,
        catalog,
        revenue_account.clone(),
        Arc::new(game_state),
    );

    let state = AppState::new(Arc::new(store), auth_key);
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
