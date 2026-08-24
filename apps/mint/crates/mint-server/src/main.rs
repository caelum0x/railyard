//! `mint-server` binary: load config from the environment, pick the store impl
//! (`PgStore` when `DATABASE_URL` is set — the production path), run migrations,
//! build the router, and serve.
//!
//! Env:
//! - `MINT_BIND_ADDR`   — listen addr (default `0.0.0.0:8080`).
//! - `DATABASE_URL`     — Postgres DSN. When set → `PgStore` (production).
//! - `MINT_API_KEY`     — required `X-API-Key` for all non-health routes.
//! - `MINT_SIGNING_KEY` — hex 32-byte receipt-signing seed (required with Postgres).
//! - `MINT_PAYER_KEYS_JSON` — JSON payer-key registrations for AITP-402.
//! - `STRIPE_WEBHOOK_SECRET` — HMAC secret for the Stripe on-ramp webhook.
//! - `MINT_ALLOW_INMEMORY=1` — dev-only escape hatch when `DATABASE_URL` is unset.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{bail, Context};
use ed25519_dalek::VerifyingKey;
use mint_payments::{HttpTransport, PaymentRegistry, ReqwestTransport};
use mint_protocol::InMemoryPayerKeys;
use mint_receipts::{Ed25519Signer, FixedSigner, Signer};
use mint_reserve::{FakeKycGate, KycGate, KycTier, Ramp};
use mint_server::{build_router, AppState};
use mint_store::{Clock, LedgerStore, SystemClock};
use mint_store_postgres::PgStore;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let bind: SocketAddr = std::env::var("MINT_BIND_ADDR")
        .unwrap_or_else(|_| "0.0.0.0:8080".to_string())
        .parse()
        .context("MINT_BIND_ADDR must be a valid socket address")?;
    let api_key = std::env::var("MINT_API_KEY").unwrap_or_default();
    if api_key.is_empty() {
        bail!("MINT_API_KEY must be set (no unauthenticated deployments)");
    }
    let stripe_secret = std::env::var("STRIPE_WEBHOOK_SECRET").unwrap_or_default();

    let database_url = std::env::var("DATABASE_URL")
        .ok()
        .filter(|url| !url.trim().is_empty());
    let signer = load_signer(database_url.is_some())?;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);

    // BYOK payment registry: every provider whose keys are present in the
    // environment gets a live adapter; nothing configured ⇒ a deterministic
    // FakeRamp (dev). The composite registry ramp is the reserve's deposit gate
    // (it routes `confirm_deposit` by the `<provider>:` ref prefix), and the
    // registry itself drives the per-provider on-ramp webhook routes.
    let transport: Arc<dyn HttpTransport> = Arc::new(ReqwestTransport::new());
    let payments = Arc::new(PaymentRegistry::from_env(transport));
    tracing::info!(
        active_providers = payments.active_count(),
        "payment registry loaded"
    );
    let ramp: Arc<dyn Ramp> = payments.ramp();
    let kyc: Arc<dyn KycGate> = Arc::new(FakeKycGate::new(KycTier::Basic, i64::MAX));
    let payer_keys = Arc::new(load_payer_keys()?);

    match database_url {
        Some(url) => {
            tracing::info!("starting with Postgres store");
            let store = PgStore::connect(&url, signer)
                .await
                .context("connecting to Postgres")?;
            store.run_migrations().await.context("running migrations")?;
            let state = AppState::new(store, clock, ramp, kyc, payer_keys, api_key, stripe_secret)
                .with_payments(payments);
            serve(state, bind).await
        }
        _ => {
            if std::env::var("MINT_ALLOW_INMEMORY").ok().as_deref() != Some("1") {
                bail!(
                    "DATABASE_URL is required in production. Set MINT_ALLOW_INMEMORY=1 for a \
                     dev-only in-memory ledger."
                );
            }
            tracing::warn!("DATABASE_URL unset — using EPHEMERAL in-memory store (dev only)");
            let store = mint_store::InMemoryStore::with_clock_and_signer(clock.clone(), signer);
            let state = AppState::new(store, clock, ramp, kyc, payer_keys, api_key, stripe_secret)
                .with_payments(payments);
            serve(state, bind).await
        }
    }
}

/// Bind + serve the router for a concrete store type.
async fn serve<S: LedgerStore + 'static>(
    state: AppState<S>,
    bind: SocketAddr,
) -> anyhow::Result<()> {
    let app = build_router(state);
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!(%bind, "mint-server listening");
    axum::serve(listener, app)
        .await
        .context("axum serve failed")?;
    Ok(())
}

/// Build the receipt signer from `MINT_SIGNING_KEY` (hex 32-byte seed), else fall
/// back to the deterministic fixed signer (dev/test).
fn load_signer(production: bool) -> anyhow::Result<Arc<dyn Signer>> {
    match std::env::var("MINT_SIGNING_KEY") {
        Ok(hex_seed) if !hex_seed.is_empty() => {
            let bytes = hex::decode(hex_seed.trim()).context("MINT_SIGNING_KEY must be hex")?;
            let seed: [u8; 32] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| anyhow::anyhow!("MINT_SIGNING_KEY must decode to 32 bytes"))?;
            Ok(Arc::new(Ed25519Signer::from_seed(
                seed,
                "mint-verifier-key-1",
            )))
        }
        _ => {
            if production {
                bail!("MINT_SIGNING_KEY is required when DATABASE_URL is configured");
            }
            tracing::warn!("MINT_SIGNING_KEY unset — using deterministic FixedSigner");
            Ok(Arc::new(FixedSigner::new()))
        }
    }
}

#[derive(serde::Deserialize)]
struct PayerKeyConfig {
    payer: String,
    pubkey_id: String,
    public_key_hex: String,
}

/// Load the public keys that authorize AITP-402 spend. Keeping this registry in
/// config makes the standalone server usable without adding a public key-admin
/// endpoint; production deployments should source the JSON from a secret/config
/// manager and restart on key rotation.
fn load_payer_keys() -> anyhow::Result<InMemoryPayerKeys> {
    let Some(raw) = std::env::var("MINT_PAYER_KEYS_JSON")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        tracing::warn!(
            "MINT_PAYER_KEYS_JSON unset — AITP-402 authorizations will reject unknown payer keys"
        );
        return Ok(InMemoryPayerKeys::new());
    };

    let configs: Vec<PayerKeyConfig> =
        serde_json::from_str(&raw).context("MINT_PAYER_KEYS_JSON must be a JSON array")?;
    let mut keys = InMemoryPayerKeys::new();
    for config in configs {
        if config.payer.trim().is_empty() || config.pubkey_id.trim().is_empty() {
            bail!("MINT_PAYER_KEYS_JSON payer and pubkey_id must be non-empty");
        }
        let decoded = hex::decode(config.public_key_hex.trim())
            .context("MINT_PAYER_KEYS_JSON public_key_hex must be hex")?;
        let bytes: [u8; 32] = decoded.as_slice().try_into().map_err(|_| {
            anyhow::anyhow!("MINT_PAYER_KEYS_JSON public_key_hex must decode to 32 bytes")
        })?;
        let verifying_key = VerifyingKey::from_bytes(&bytes)
            .context("MINT_PAYER_KEYS_JSON contains an invalid Ed25519 public key")?;
        keys.insert(config.payer, config.pubkey_id, verifying_key);
    }
    Ok(keys)
}

fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .init();
}
