//! Server wiring. Two store backends are selectable via `PASSPORT_STORE`:
//!
//!   * `postgres` (default) — the production path. Fail-closed on missing config:
//!     no deployment starts without its database, API key, and Relay credentials.
//!   * `memory` — an embedded, non-durable store for local dev and demos. It runs
//!     the real router / service / auth against an in-memory store so the full
//!     create → edit → lock → resolve lifecycle is drivable with no external
//!     infrastructure. Data is lost on restart; never use it in production.
//!
//! The Relay seam is real in both modes: if `RELAY_BASE`/`RELAY_API_KEY` are set,
//! the receipted [`RelayClient`] is injected. In `memory` mode they are optional —
//! when absent, an [`UnconfiguredRelay`] is injected that fails closed on every
//! generation attempt (it never fabricates settlement evidence), so `generate`
//! honestly returns `502 relay_unavailable` until a real Relay is configured.
//!
//! Access: `PASSPORT_API_KEY` is the operator key; `PASSPORT_OWNER_KEYS`
//! (`owner:key,...`) adds per-owner tenant keys scoped to their own passports.
//! Receipts: `MINT_RECEIPT_PUBLIC_KEYS` (hex Ed25519, comma-separated) or
//! `MINT_BASE_URL` (keys discovered from `/.well-known/mint-verification-keys.json`).
//! The operator console is served at `GET /console`.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{bail, Context};
use async_trait::async_trait;

use passport_core::{
    PassportError, RelayConditioningSink, RelayGenerationRequest, RelayGenerationResult,
};
use passport_relay_client::RelayClient;
use passport_server::service::PassportService;
use passport_server::auth::OwnerKeys;
use passport_server::receipts::ReceiptVerifier;
use passport_server::{build_router, AppState};
use passport_store::MemoryStore;
use passport_store_postgres::PostgresStore;

/// Which persistence backend the server binds at startup.
enum StoreMode {
    /// Durable Neon/Postgres store (production default).
    Postgres { database_url: String },
    /// Non-durable embedded store for local dev and demos.
    Memory,
}

/// Optional Relay transport credentials.
struct RelayConfig {
    base: String,
    key: String,
}

enum ReceiptKeys {
    Static(Vec<String>),
    Discover(String),
    None,
}

struct Config {
    bind_addr: SocketAddr,
    api_key: String,
    owner_keys: OwnerKeys,
    receipt_keys: ReceiptKeys,
    store: StoreMode,
    relay: Option<RelayConfig>,
}

impl Config {
    fn from_env() -> anyhow::Result<Self> {
        let bind_addr = std::env::var("PASSPORT_BIND_ADDR")
            .unwrap_or_else(|_| "0.0.0.0:8095".to_string())
            .parse()
            .context("PASSPORT_BIND_ADDR must be a valid socket address")?;
        let api_key = required("PASSPORT_API_KEY")?;

        let store = match std::env::var("PASSPORT_STORE")
            .unwrap_or_else(|_| "postgres".to_string())
            .to_ascii_lowercase()
            .as_str()
        {
            "postgres" => StoreMode::Postgres {
                database_url: required("DATABASE_URL")?,
            },
            "memory" => StoreMode::Memory,
            other => bail!("PASSPORT_STORE must be 'postgres' or 'memory', got '{other}'"),
        };

        // Relay is mandatory in production (postgres) and optional in memory mode
        // so the lifecycle can be exercised without a live Relay gateway.
        let relay = match (optional("RELAY_BASE"), optional("RELAY_API_KEY")) {
            (Some(base), Some(key)) => Some(RelayConfig { base, key }),
            (None, None) => {
                if matches!(store, StoreMode::Postgres { .. }) {
                    bail!("RELAY_BASE and RELAY_API_KEY must be set");
                }
                None
            }
            _ => bail!("RELAY_BASE and RELAY_API_KEY must both be set, or both be omitted"),
        };

        let owner_keys = match optional("PASSPORT_OWNER_KEYS") {
            Some(spec) => OwnerKeys::parse(&spec).map_err(|e| anyhow::anyhow!("PASSPORT_OWNER_KEYS: {e}"))?,
            None => OwnerKeys::default(),
        };

        // Mint receipt keys: explicit hex keys win; otherwise discover them from
        // the live Mint. Durable (postgres) deployments must verify receipts.
        let receipt_keys = match (
            optional("MINT_RECEIPT_PUBLIC_KEYS"),
            optional("MINT_BASE_URL"),
        ) {
            (Some(keys), _) => ReceiptKeys::Static(
                keys.split(',').map(|k| k.trim().to_string()).collect(),
            ),
            (None, Some(base)) => ReceiptKeys::Discover(base),
            (None, None) => {
                if matches!(store, StoreMode::Postgres { .. }) {
                    bail!("MINT_RECEIPT_PUBLIC_KEYS or MINT_BASE_URL must be set so settlement receipts can be verified");
                }
                ReceiptKeys::None
            }
        };

        Ok(Self {
            bind_addr,
            api_key,
            owner_keys,
            receipt_keys,
            store,
            relay,
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let config = Config::from_env()?;

    let relay: Arc<dyn RelayConditioningSink> = match &config.relay {
        Some(cfg) => Arc::new(RelayClient::new(cfg.base.clone(), cfg.key.clone())),
        None => Arc::new(UnconfiguredRelay),
    };

    let service = match &config.store {
        StoreMode::Postgres { database_url } => {
            let store = PostgresStore::connect(database_url)
                .await
                .context("connecting to Passport Postgres")?;
            store
                .migrate()
                .await
                .context("running Passport migrations")?;
            let store = Arc::new(store);
            tracing::info!("passport store: postgres (durable)");
            PassportService::new(store.clone(), store.clone(), relay).with_artifact_store(store)
        }
        StoreMode::Memory => {
            let store = Arc::new(MemoryStore::default());
            tracing::warn!(
                "passport store: memory (NON-DURABLE — dev/demo only; data is lost on restart)"
            );
            if config.relay.is_none() {
                tracing::warn!(
                    "relay: unconfigured — `generate` will return 502 until RELAY_BASE/RELAY_API_KEY are set"
                );
            }
            PassportService::new(store.clone(), store.clone(), relay).with_artifact_store(store)
        }
    };

    let service = match &config.receipt_keys {
        ReceiptKeys::Static(keys) => {
            let verifier = ReceiptVerifier::from_hex_keys(keys)
                .map_err(|e| anyhow::anyhow!("MINT_RECEIPT_PUBLIC_KEYS: {e}"))?;
            service.with_receipt_verifier(Arc::new(verifier))
        }
        ReceiptKeys::Discover(base) => {
            let verifier = ReceiptVerifier::fetch(base)
                .await
                .map_err(|e| anyhow::anyhow!("discovering Mint receipt keys: {e}"))?;
            tracing::info!(mint = %base, "mint receipt keys discovered");
            service.with_receipt_verifier(Arc::new(verifier))
        }
        ReceiptKeys::None => {
            tracing::warn!(
                "mint receipt keys unconfigured — /attest is disabled until MINT_RECEIPT_PUBLIC_KEYS or MINT_BASE_URL is set"
            );
            service
        }
    };

    let state = AppState {
        service,
        api_key: Arc::from(config.api_key),
        owner_keys: Arc::new(config.owner_keys),
    };

    let listener = tokio::net::TcpListener::bind(config.bind_addr)
        .await
        .with_context(|| format!("binding {}", config.bind_addr))?;
    tracing::info!(bind = %config.bind_addr, "identity-lock-server listening");
    axum::serve(listener, build_router(state))
        .await
        .context("Passport server failed")?;
    Ok(())
}

/// A Relay sink used only in `memory` mode when no Relay is configured. It fails
/// closed on every call — it never invents an output reference or settlement
/// evidence — so a generation cannot be recorded without a real, receipted Relay.
struct UnconfiguredRelay;

#[async_trait]
impl RelayConditioningSink for UnconfiguredRelay {
    async fn generate(
        &self,
        _request: &RelayGenerationRequest,
    ) -> Result<RelayGenerationResult, PassportError> {
        Err(PassportError::Relay(
            "Relay is not configured; set RELAY_BASE and RELAY_API_KEY to enable generation".into(),
        ))
    }
}

fn required(name: &str) -> anyhow::Result<String> {
    match optional(name) {
        Some(value) => Ok(value),
        None => bail!("{name} must be set"),
    }
}

fn optional(name: &str) -> Option<String> {
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).init();
}
