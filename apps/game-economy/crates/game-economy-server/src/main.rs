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
//! - `POST /v1/entitlements/revoke`  — chargeback revocation, no money moved (game-server key).
//! - `POST /v1/refunds`              — refund through Mint + revoke (game-server key).
//! - `GET  /admin`, `/v1/admin/*`    — operator console (GAME_ADMIN_TOKEN).
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
//! - `GAME_DATABASE_URL` / `DATABASE_URL` — Postgres state (multi-instance safe; migrations run at boot). When unset, the single-instance JSON file below is used.
//! - `GAME_DATABASE_MAX_CONNECTIONS` — Postgres pool size per instance (default 10).
//! - `GAME_STATE_PATH`         — JSON durability file (default `./game-economy-state.json`), single instance only.
//! - `GAME_ADMIN_TOKEN`        — operator console bearer token (>= 24 chars; console API answers 503 until set).
//! - `GAME_BIND_ADDR`          — listen address (default `0.0.0.0:8102`).

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;

use game_economy_core::{Catalog, GameStateRepository, GameStore};
use game_economy_server::fulfill::{ServerFulfiller, WebhookFulfiller};
use game_economy_mint_client::HttpMintTransport;
use game_economy_server::{auth, build_router, AppState};
use game_economy_store::JsonFileGameState;
use game_economy_store_postgres::PostgresGameState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let revenue_account = required_env("GAME_REVENUE_ACCOUNT")?;
    // Auth is mandatory: refuse to boot an unauthenticated storefront (the money
    // path would otherwise charge any account named in the request body).
    let auth_secret = required_env("GAME_AUTH_SECRET")?;
    let auth_key = auth::derive_key(auth_secret.as_bytes());
    let bind: SocketAddr = std::env::var("GAME_BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8102".to_string())
        .parse()
        .context("GAME_BIND_ADDR must be a valid socket address")?;

    // Real Mint client from the environment (RAILYARD_MINT_URL + MINT_API_KEY).
    let mint = HttpMintTransport::from_env()
        .map_err(|e| anyhow::anyhow!("configuring Mint transport: {e}"))?;

    let (game_state, state_backend) = load_game_state().await?;

    let catalog = load_catalog()?;
    let fulfiller = load_fulfiller()?;
    let store = GameStore::with_state(
        mint,
        fulfiller,
        catalog,
        revenue_account.clone(),
        game_state,
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
    match std::env::var("GAME_ADMIN_TOKEN") {
        Ok(token) if token.trim().len() >= 24 => {
            state = state.with_admin_token(token.trim().as_bytes());
        }
        Ok(_) => anyhow::bail!("GAME_ADMIN_TOKEN must be at least 24 characters"),
        Err(_) => tracing::warn!(
            "GAME_ADMIN_TOKEN unset: the operator console (/admin, /v1/admin/*) is disabled (503)"
        ),
    }
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(%bind, revenue_account, state_backend, "game-economy-server listening");
    axum::serve(listener, app)
        .await
        .context("axum serve failed")?;
    Ok(())
}

/// Pick the durable state backend: Postgres when `GAME_DATABASE_URL` /
/// `DATABASE_URL` is set (required for more than one instance), otherwise the
/// single-instance JSON file at `GAME_STATE_PATH`.
async fn load_game_state() -> anyhow::Result<(Arc<dyn GameStateRepository>, String)> {
    let database_url = std::env::var("GAME_DATABASE_URL")
        .or_else(|_| std::env::var("DATABASE_URL"))
        .ok()
        .filter(|v| !v.trim().is_empty());
    if let Some(url) = database_url {
        let max_connections: u32 = std::env::var("GAME_DATABASE_MAX_CONNECTIONS")
            .ok()
            .map(|v| v.trim().parse())
            .transpose()
            .context("GAME_DATABASE_MAX_CONNECTIONS must be a positive integer")?
            .unwrap_or(game_economy_store_postgres::DEFAULT_MAX_CONNECTIONS);
        let pg = PostgresGameState::connect_with(url.trim(), max_connections)
            .await
            .map_err(|e| anyhow::anyhow!("connecting game state database: {e}"))?;
        pg.migrate()
            .await
            .map_err(|e| anyhow::anyhow!("migrating game state database: {e}"))?;
        tracing::info!("game state: postgres (multi-instance safe)");
        return Ok((Arc::new(pg), "postgres".to_string()));
    }
    let state_path = std::env::var("GAME_STATE_PATH")
        .unwrap_or_else(|_| "./game-economy-state.json".to_string());
    let json = JsonFileGameState::open(&state_path)
        .map_err(|e| anyhow::anyhow!("opening game state {state_path}: {e}"))?;
    tracing::warn!(
        state_path,
        "game state: single-instance JSON file; set GAME_DATABASE_URL before running more than one instance"
    );
    Ok((Arc::new(json), format!("json:{state_path}")))
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
