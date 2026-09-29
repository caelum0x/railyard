//! `mint-server` binary: load config from the environment, pick the store impl
//! (`PgStore` when `DATABASE_URL` is set — the production path), run migrations,
//! build the router, and serve.
//!
//! Env:
//! - `MINT_BIND_ADDR`   — listen addr (default `0.0.0.0:8080`).
//! - `DATABASE_URL`     — Postgres DSN. When set → `PgStore` (production).
//! - `MINT_API_KEY`     — master `X-API-Key` (full authority) for all API routes.
//! - `MINT_API_KEYS_JSON` — optional named, scoped service keys:
//!   `[{"name":"ledgerline","key":"…","scopes":["read","spend"],"accounts":["acct_…"]}]`;
//!   scopes are `read | accounts | spend | funding | payouts | admin`; the
//!   optional `accounts` list restricts the key to those accounts. Keys can
//!   also be created / revoked at runtime (`/v1/keys`, stored hashed).
//! - `MINT_SIGNING_KEY` — hex 32-byte receipt-signing seed (required with Postgres).
//! - `MINT_PAYER_KEYS_JSON` — JSON payer-key registrations for AITP-402.
//! - `STRIPE_WEBHOOK_SECRET` — HMAC secret for the Stripe on-ramp webhook.
//! - `MINT_ALLOW_INMEMORY=1` — dev-only escape hatch when `DATABASE_URL` is unset.
//! - `MINT_KYC_PROVIDER` — `persona` | `stripe_identity`: install a real,
//!   provider-verified KYC gate (`VerifiedKycGate`) for cash-out. Requires the
//!   provider key (`PERSONA_API_KEY`, or `STRIPE_API_KEY` for `stripe_identity`);
//!   if the key is absent the server logs and falls back to the deny-by-default
//!   `StrictKycGate` (fail closed). Unset ⇒ redemptions stay denied by default.
//! - `MINT_KYC_PER_TX_LIMIT_MICROS` — per-redemption payout ceiling in
//!   microdollars for the real gate (default `1_000_000_000` = $1,000).
//! - `MINT_KYC_MIN_TIER` — `basic` | `verified`: the minimum provider-verified
//!   tier a redemption requires (default `verified`).
//! - `MINT_DEV_KYC_ALLOW=1` — dev-only: install the permissive `FakeKycGate`
//!   instead of the deny-by-default `StrictKycGate`. REFUSED in production
//!   (`DATABASE_URL` set) so it cannot ship enabled on a money-moving deployment.
//! - `PERSONA_INQUIRY_TEMPLATE_ID` — Persona inquiry template (`itmpl_…`) used by
//!   `POST /v1/accounts/:id/kyc/session` to start a hosted verification.
//! - `MINT_PUBLIC_URL` — Mint's externally reachable base URL; hosts the
//!   `/checkout/success|cancel` pages and the PayPal capture return. Required for
//!   PayPal checkouts.
//! - `MINT_CHECKOUT_SUCCESS_URL` / `MINT_CHECKOUT_CANCEL_URL` — default buyer
//!   landing pages after a hosted checkout (a request may override them).
//! - `MINT_DEV_FUNDING=1` — dev-only: explicitly opt into the in-memory dev
//!   funding ramp when no payment provider is configured. Without it, a
//!   production deployment with no provider refuses to start rather than silently
//!   funding against a fake ramp.
//! - `MINT_SHADOW=1` — shadow mode (RY-MT1): production store, API, receipts and
//!   sweeps, but a ramp that refuses every deposit and payout, so no money moves.
//!   Refused together with a configured payment provider or `MINT_DEV_FUNDING=1`.
//! - `MINT_METRICS_TOKEN` — bearer token for `GET /metrics`; unset ⇒ only direct
//!   (non-proxied) scrapes are served.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{bail, Context};
use ed25519_dalek::VerifyingKey;
use mint_payments::{
    HttpTransport, KycOnboarding, PaymentRegistry, PersonaKycVerifier, ReqwestTransport,
    StripeIdentityKycVerifier,
};
use mint_protocol::InMemoryPayerKeys;
use mint_receipts::{Ed25519Signer, FixedSigner, Signer};
use mint_reserve::{FakeKycGate, KycGate, KycTier, Ramp, StrictKycGate, VerifiedKycGate};
use mint_server::{build_router, AppState, PublicConfig};
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
    let scoped_keys = match env_nonempty("MINT_API_KEYS_JSON") {
        Some(raw) => mint_server::auth::parse_scoped_keys(&raw).map_err(anyhow::Error::msg)?,
        None => Vec::new(),
    };
    tracing::info!(scoped_keys = scoped_keys.len(), "API keys loaded");

    let database_url = std::env::var("DATABASE_URL")
        .ok()
        .filter(|url| !url.trim().is_empty());
    // A configured Postgres DSN is the production signal. It hardens the signer,
    // the KYC gate, and the funding ramp all in lockstep.
    let production = database_url.is_some();
    let signer = load_signer(production)?;
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);

    // BYOK payment registry: every provider whose keys are present in the
    // environment gets a live adapter; nothing configured ⇒ a deterministic
    // FakeRamp (dev). The composite registry ramp is the reserve's deposit gate
    // (it routes `confirm_deposit` by the `<provider>:` ref prefix), and the
    // registry itself drives the per-provider on-ramp webhook routes.
    let transport: Arc<dyn HttpTransport> = Arc::new(ReqwestTransport::new());
    let payments = Arc::new(PaymentRegistry::from_env(transport.clone()));
    tracing::info!(
        active_providers = payments.active_count(),
        "payment registry loaded"
    );
    let ramp: Arc<dyn Ramp> = resolve_ramp(&payments, production)?;
    let mirror_transport = transport.clone();
    let (kyc, kyc_onboarding) = load_kyc_gate(production, transport)?;
    let mirror_keys = match env_nonempty("MINT_MIRROR_KEYS_JSON") {
        Some(raw) => mint_server::mirror::parse_keys(&raw).map_err(anyhow::Error::msg)?,
        None => Vec::new(),
    };
    let mirror_endpoints = match env_nonempty("MINT_EVENT_WEBHOOKS_JSON") {
        Some(raw) => mint_server::mirror::parse_endpoints(&raw).map_err(anyhow::Error::msg)?,
        None => Vec::new(),
    };
    let mirror_state = |store: Arc<dyn mint_store::MirrorStore>| mint_server::mirror::MirrorState {
        store,
        keys: Arc::new(mirror_keys.clone()),
        endpoints: Arc::new(mirror_endpoints.clone()),
        transport: mirror_transport.clone(),
        now_ms: mint_server::mirror::system_now_ms,
    };
    // Seller-own payments (Wave P PY2.3): product keys, the sealing key and,
    // only with all three Connect values (OD-9), Stripe Connect.
    let seller_products = match env_nonempty("MINT_SELLER_PRODUCTS_JSON") {
        Some(raw) => mint_server::seller::parse_products(&raw).map_err(anyhow::Error::msg)?,
        None => Vec::new(),
    };
    let seller_seal = match env_nonempty("MINT_SELLER_SEAL_KEY") {
        Some(raw) => Some(Arc::new(mint_payments::seller::seal::SealKey::from_base64(&raw).map_err(anyhow::Error::msg)?)),
        None => None,
    };
    let seller_stripe = mint_server::seller::stripe_from_env(
        env_nonempty("MINT_STRIPE_CONNECT_CLIENT_ID"),
        env_nonempty("MINT_STRIPE_PLATFORM_SECRET_KEY"),
        env_nonempty("MINT_STRIPE_CONNECT_WEBHOOK_SECRET"),
    );
    let seller_public = env_nonempty("MINT_PUBLIC_URL").unwrap_or_default();
    let seller_transport = mirror_transport.clone();
    let seller_state = |store: Arc<dyn mint_store::SellerStore>| mint_server::seller::SellerState {
        store,
        products: Arc::new(seller_products.clone()),
        seal: seller_seal.clone(),
        transport: seller_transport.clone(),
        public_url: seller_public.clone(),
        stripe: seller_stripe.clone(),
        now_ms: mint_server::mirror::system_now_ms,
    };
    let public = PublicConfig::from_env();
    if public.public_url.is_none() && public.checkout_success_url.is_none() {
        tracing::warn!(
            "neither MINT_PUBLIC_URL nor MINT_CHECKOUT_SUCCESS_URL is set: hosted checkouts must \
             pass success_url/cancel_url per request, and PayPal checkouts are disabled"
        );
    }
    let payer_keys = Arc::new(load_payer_keys()?);

    match database_url {
        Some(url) => {
            tracing::info!("starting with Postgres store");
            let store = PgStore::connect(&url, signer.clone())
                .await
                .context("connecting to Postgres")?;
            store.run_migrations().await.context("running migrations")?;
            let mirror = mirror_state(Arc::new(mint_store_postgres::PgMirror::new(store.pool().clone())));
            let sellers = seller_state(Arc::new(mint_store_postgres::PgSeller::new(store.pool().clone())));
            let state = AppState::new(
                store,
                clock,
                ramp,
                kyc,
                payer_keys,
                signer,
                api_key,
                stripe_secret,
            )
            .with_payments(payments)
            .with_public_config(public)
            .with_scoped_keys(scoped_keys);
            let state = match kyc_onboarding {
                Some(k) => state.with_kyc_onboarding(k),
                None => state,
            };
            serve(state, bind, mirror, sellers).await
        }
        _ => {
            if std::env::var("MINT_ALLOW_INMEMORY").ok().as_deref() != Some("1") {
                bail!(
                    "DATABASE_URL is required in production. Set MINT_ALLOW_INMEMORY=1 for a \
                     dev-only in-memory ledger."
                );
            }
            tracing::warn!("DATABASE_URL unset — using EPHEMERAL in-memory store (dev only)");
            let store =
                mint_store::InMemoryStore::with_clock_and_signer(clock.clone(), signer.clone());
            let mirror = mirror_state(Arc::new(mint_store::MemoryMirror::default()));
            let sellers = seller_state(Arc::new(mint_store::MemorySellerStore::default()));
            let state = AppState::new(
                store,
                clock,
                ramp,
                kyc,
                payer_keys,
                signer,
                api_key,
                stripe_secret,
            )
            .with_payments(payments)
            .with_public_config(public)
            .with_scoped_keys(scoped_keys);
            let state = match kyc_onboarding {
                Some(k) => state.with_kyc_onboarding(k),
                None => state,
            };
            serve(state, bind, mirror, sellers).await
        }
    }
}

/// Bind + serve the router for a concrete store type.
async fn serve<S: LedgerStore + 'static>(
    state: AppState<S>,
    bind: SocketAddr,
    mirror: mint_server::mirror::MirrorState,
    sellers: mint_server::seller::SellerState,
) -> anyhow::Result<()> {
    // Mirror the DB-backed AITP payer keys before serving, then keep payouts,
    // hold expiries and the key mirror reconciled in the background.
    match state.payer_keys.reload(state.ledger.store()).await {
        Ok(n) => tracing::info!(active_db_payer_keys = n, "AITP payer keys loaded"),
        Err(e) => tracing::error!(error = %e, "loading AITP payer keys from the store failed"),
    }
    mint_server::jobs::spawn_background_jobs(state.clone());
    // The portfolio mirror (Wave P PY1.5): its own routes and product keys, no ledger handle.
    if std::env::var("MINT_SWEEP_DISABLED").ok().as_deref() != Some("1") {
        mint_server::mirror::spawn_delivery(mirror.clone());
        // Seller-own payments (Wave P PY2.3): entitlement events to the calling products.
        mint_server::seller::spawn_delivery(sellers.clone());
    }
    let app = build_router(state)
        .merge(mint_server::mirror::router(mirror))
        .merge(mint_server::seller::router(sellers));
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

/// A real, provider-verified KYC backend the binary can install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KycProvider {
    /// Persona (`withpersona.com`) inquiries.
    Persona,
    /// Stripe Identity verification sessions.
    StripeIdentity,
}

impl KycProvider {
    /// Parse the `MINT_KYC_PROVIDER` value (case-insensitive). `None` ⇒ the value
    /// is not a recognized provider.
    fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "persona" => Some(KycProvider::Persona),
            "stripe" | "stripe_identity" | "stripe-identity" => Some(KycProvider::StripeIdentity),
            _ => None,
        }
    }
}

/// Which KYC gate the binary should install. Decided purely from the deployment
/// mode, the dev override, and the configured provider so the decision is
/// unit-testable without touching the process environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KycChoice {
    /// Deny-by-default [`StrictKycGate`] — the fail-closed default.
    Strict,
    /// Permissive [`FakeKycGate`] — dev only, never in production.
    DevFake,
    /// A real, provider-verified [`VerifiedKycGate`].
    Provider(KycProvider),
}

/// Pure KYC selection. Precedence: the dev fake (dev only, refused in prod) →
/// a real provider (only when its key is present) → deny-by-default. A provider
/// named without its key falls back to Strict (fail closed) rather than crashing
/// the whole server, since only cash-out — not deposits/transfers — is gated.
fn choose_kyc(
    production: bool,
    dev_allow: bool,
    provider: Option<KycProvider>,
    provider_key_present: bool,
) -> anyhow::Result<KycChoice> {
    if production && dev_allow {
        bail!(
            "MINT_DEV_KYC_ALLOW=1 must not be set in production (DATABASE_URL is configured): the \
             permissive dev KYC gate cannot ship enabled on a money-moving deployment. Wire a real \
             KycGate provider (MINT_KYC_PROVIDER) instead."
        );
    }
    if dev_allow {
        return Ok(KycChoice::DevFake);
    }
    if let Some(p) = provider {
        if provider_key_present {
            return Ok(KycChoice::Provider(p));
        }
        return Ok(KycChoice::Strict);
    }
    Ok(KycChoice::Strict)
}

/// Which funding ramp the binary should install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RampChoice {
    /// The real composite ramp built from configured providers.
    Real,
    /// The in-memory dev [`FakeRamp`] the registry falls back to — dev only.
    DevFake,
    /// Shadow mode: the refusing [`mint_server::shadow::ShadowRamp`].
    Shadow,
}

/// Pure funding-ramp selection: a configured provider is always used; the dev
/// fake fallback is refused in production unless explicitly opted into.
fn choose_ramp(
    production: bool,
    has_provider: bool,
    dev_funding: bool,
    shadow: bool,
) -> anyhow::Result<RampChoice> {
    if shadow {
        if has_provider {
            bail!(
                "MINT_SHADOW=1 with a payment provider configured: a hosted checkout could take a \
                 buyer's money that shadow mode would never credit. Remove the provider keys or \
                 unset MINT_SHADOW."
            );
        }
        if dev_funding {
            bail!("MINT_SHADOW=1 and MINT_DEV_FUNDING=1 are mutually exclusive");
        }
        return Ok(RampChoice::Shadow);
    }
    if has_provider {
        return Ok(RampChoice::Real);
    }
    if production && !dev_funding {
        bail!(
            "no payment provider configured but DATABASE_URL is set: refusing to start with the \
             in-memory dev funding ramp in production. Configure a provider (e.g. STRIPE_*), or set \
             MINT_DEV_FUNDING=1 to explicitly opt into the dev ramp for testing."
        );
    }
    Ok(RampChoice::DevFake)
}

/// The default per-redemption payout ceiling for the real KYC gate: $1,000.
const DEFAULT_KYC_PER_TX_LIMIT_MICROS: i64 = 1_000_000_000;

/// Read an env var, treating empty/whitespace as unset.
fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// The per-tx payout ceiling (microdollars) for the real KYC gate.
fn kyc_per_tx_limit() -> anyhow::Result<i64> {
    match env_nonempty("MINT_KYC_PER_TX_LIMIT_MICROS") {
        Some(raw) => {
            let n: i64 = raw
                .trim()
                .parse()
                .context("MINT_KYC_PER_TX_LIMIT_MICROS must be an integer microdollar amount")?;
            if n <= 0 {
                bail!("MINT_KYC_PER_TX_LIMIT_MICROS must be a positive microdollar amount");
            }
            Ok(n)
        }
        None => Ok(DEFAULT_KYC_PER_TX_LIMIT_MICROS),
    }
}

/// The minimum provider-verified tier a redemption requires for the real gate.
fn kyc_min_tier() -> anyhow::Result<KycTier> {
    match env_nonempty("MINT_KYC_MIN_TIER") {
        Some(raw) => match raw.trim().to_ascii_lowercase().as_str() {
            "basic" => Ok(KycTier::Basic),
            "verified" => Ok(KycTier::Verified),
            other => bail!("MINT_KYC_MIN_TIER must be 'basic' or 'verified' (got '{other}')"),
        },
        None => Ok(KycTier::Verified),
    }
}

/// Select the KYC gate from the environment. Precedence:
/// 1. A real provider (`MINT_KYC_PROVIDER` + its key) → [`VerifiedKycGate`],
///    which authorizes against provider-verified identity + a payout limit.
/// 2. `MINT_DEV_KYC_ALLOW=1` (dev only, refused in production) → the permissive
///    [`FakeKycGate`].
/// 3. Otherwise the deny-by-default [`StrictKycGate`].
/// The installed redemption gate plus, when a real provider is configured, the
/// onboarding client used to start verifications and report status.
type KycWiring = (Arc<dyn KycGate>, Option<Arc<dyn KycOnboarding>>);

fn load_kyc_gate(
    production: bool,
    transport: Arc<dyn HttpTransport>,
) -> anyhow::Result<KycWiring> {
    let dev_allow = std::env::var("MINT_DEV_KYC_ALLOW").ok().as_deref() == Some("1");
    let provider_raw = env_nonempty("MINT_KYC_PROVIDER");
    let provider = match &provider_raw {
        Some(raw) => Some(KycProvider::parse(raw).with_context(|| {
            format!("MINT_KYC_PROVIDER '{raw}' is invalid — must be one of: persona, stripe_identity")
        })?),
        None => None,
    };
    // Each provider draws its credential from a distinct env var.
    let provider_key = match provider {
        Some(KycProvider::Persona) => env_nonempty("PERSONA_API_KEY"),
        Some(KycProvider::StripeIdentity) => env_nonempty("STRIPE_API_KEY"),
        None => None,
    };
    let min_tier = kyc_min_tier()?;
    let per_tx_limit = kyc_per_tx_limit()?;

    match choose_kyc(production, dev_allow, provider, provider_key.is_some())? {
        KycChoice::Provider(p) => {
            let key = provider_key.expect("provider key presence checked by choose_kyc");
            let (gate, onboarding): KycWiring = match p {
                KycProvider::Persona => {
                    let template = env_nonempty("PERSONA_INQUIRY_TEMPLATE_ID");
                    if template.is_none() {
                        tracing::warn!(
                            "PERSONA_INQUIRY_TEMPLATE_ID unset: KYC status works, but starting a \
                             new verification returns 503 until it is set"
                        );
                    }
                    (
                        Arc::new(VerifiedKycGate::new(
                            PersonaKycVerifier::new(transport.clone(), key.clone()),
                            min_tier,
                            per_tx_limit,
                        )),
                        Some(Arc::new(
                            PersonaKycVerifier::new(transport, key).with_template(template),
                        )),
                    )
                }
                KycProvider::StripeIdentity => (
                    Arc::new(VerifiedKycGate::new(
                        StripeIdentityKycVerifier::new(transport.clone(), key.clone()),
                        min_tier,
                        per_tx_limit,
                    )),
                    Some(Arc::new(StripeIdentityKycVerifier::new(transport, key))),
                ),
            };
            tracing::info!(
                provider = ?p,
                per_tx_limit_micros = per_tx_limit,
                min_tier = ?min_tier,
                "KYC: live provider gate installed (authorizes against provider-verified identity)"
            );
            Ok((gate, onboarding))
        }
        KycChoice::DevFake => {
            tracing::warn!(
                "MINT_DEV_KYC_ALLOW=1 — installing permissive FakeKycGate (dev only; approves \
                 caller-presented redemptions)"
            );
            Ok((Arc::new(FakeKycGate::new(KycTier::Basic, i64::MAX)), None))
        }
        KycChoice::Strict => {
            if provider_raw.is_some() {
                tracing::warn!(
                    "MINT_KYC_PROVIDER is set but its API key is missing — redemptions are DENIED \
                     (StrictKycGate) until the provider key is supplied"
                );
            } else {
                tracing::info!(
                    "KYC: StrictKycGate (deny-by-default) — redemptions are refused until a real \
                     KycGate provider is wired (MINT_KYC_PROVIDER)"
                );
            }
            Ok((Arc::new(StrictKycGate::new()), None))
        }
    }
}

/// Resolve the reserve funding ramp from the environment. Uses the real
/// composite ramp when a provider is configured; otherwise the registry's
/// in-memory dev [`FakeRamp`], which is refused in production unless
/// `MINT_DEV_FUNDING=1` is explicitly set.
fn resolve_ramp(payments: &PaymentRegistry, production: bool) -> anyhow::Result<Arc<dyn Ramp>> {
    let has_provider = payments.active_count() > 0;
    let dev_funding = std::env::var("MINT_DEV_FUNDING").ok().as_deref() == Some("1");
    let shadow = mint_server::shadow::shadow_enabled();
    match choose_ramp(production, has_provider, dev_funding, shadow)? {
        RampChoice::Real => Ok(payments.ramp()),
        RampChoice::Shadow => {
            tracing::warn!(
                "MINT_SHADOW=1 — shadow mode: every deposit and payout is refused; no money moves"
            );
            Ok(Arc::new(mint_server::shadow::ShadowRamp))
        }
        RampChoice::DevFake => {
            if production {
                tracing::warn!(
                    "MINT_DEV_FUNDING=1 — using the in-memory dev FakeRamp as the funding path in a \
                     production build; deposits are NOT real money"
                );
            } else {
                tracing::warn!(
                    "no payment provider configured — using in-memory dev FakeRamp (dev only)"
                );
            }
            Ok(payments.ramp())
        }
    }
}

#[derive(serde::Deserialize)]
struct PayerKeyConfig {
    payer: String,
    pubkey_id: String,
    public_key_hex: String,
}

/// Load the optional BOOTSTRAP public keys that authorize AITP-402 spend. The
/// primary registry is DB-backed (`/v1/payer-keys` + the operator console,
/// no restart needed); this env set is a static fallback layered underneath
/// it, and a key revoked in the DB is refused even if it is still listed here.
fn load_payer_keys() -> anyhow::Result<InMemoryPayerKeys> {
    let Some(raw) = std::env::var("MINT_PAYER_KEYS_JSON")
        .ok()
        .filter(|value| !value.trim().is_empty())
    else {
        tracing::info!(
            "MINT_PAYER_KEYS_JSON unset — AITP-402 payer keys come from the DB registry \
             (/v1/payer-keys) only"
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kyc_defaults_to_strict_deny_in_production() {
        // Production, no dev override, no provider → the deny-by-default gate.
        assert_eq!(choose_kyc(true, false, None, false).unwrap(), KycChoice::Strict);
    }

    #[test]
    fn kyc_defaults_to_strict_deny_in_dev_too() {
        // Even in dev, absent an explicit opt-in the gate denies (fail closed).
        assert_eq!(
            choose_kyc(false, false, None, false).unwrap(),
            KycChoice::Strict
        );
    }

    #[test]
    fn kyc_dev_fake_only_when_explicitly_allowed_in_dev() {
        assert_eq!(
            choose_kyc(false, true, None, false).unwrap(),
            KycChoice::DevFake
        );
    }

    #[test]
    fn kyc_dev_fake_refused_in_production() {
        // The permissive gate must NOT be installable on a production deployment.
        let err = choose_kyc(true, true, None, false).unwrap_err();
        assert!(err.to_string().contains("must not be set in production"));
    }

    #[test]
    fn kyc_uses_real_provider_when_key_present_even_in_production() {
        // A configured provider WITH its key is the real gate — in production too.
        assert_eq!(
            choose_kyc(true, false, Some(KycProvider::Persona), true).unwrap(),
            KycChoice::Provider(KycProvider::Persona)
        );
        assert_eq!(
            choose_kyc(false, false, Some(KycProvider::StripeIdentity), true).unwrap(),
            KycChoice::Provider(KycProvider::StripeIdentity)
        );
    }

    #[test]
    fn kyc_provider_without_key_falls_back_to_strict_deny() {
        // Named but keyless ⇒ fail closed (deny), not a fake approval.
        assert_eq!(
            choose_kyc(true, false, Some(KycProvider::Persona), false).unwrap(),
            KycChoice::Strict
        );
    }

    #[test]
    fn kyc_provider_string_parsing() {
        assert_eq!(KycProvider::parse("persona"), Some(KycProvider::Persona));
        assert_eq!(
            KycProvider::parse("Stripe_Identity"),
            Some(KycProvider::StripeIdentity)
        );
        assert_eq!(KycProvider::parse("stripe"), Some(KycProvider::StripeIdentity));
        assert_eq!(KycProvider::parse("nope"), None);
    }

    #[test]
    fn ramp_uses_real_provider_whenever_one_is_configured() {
        // A configured provider is always used, regardless of mode or dev flag.
        assert_eq!(
            choose_ramp(true, true, false, false).unwrap(),
            RampChoice::Real
        );
        assert_eq!(
            choose_ramp(false, true, true, false).unwrap(),
            RampChoice::Real
        );
    }

    #[test]
    fn ramp_dev_fake_allowed_in_dev_without_provider() {
        assert_eq!(
            choose_ramp(false, false, false, false).unwrap(),
            RampChoice::DevFake
        );
    }

    #[test]
    fn ramp_dev_fake_refused_in_production_without_opt_in() {
        let err = choose_ramp(true, false, false, false).unwrap_err();
        assert!(err.to_string().contains("refusing to start"));
    }

    #[test]
    fn ramp_shadow_refuses_money_in_production_without_a_provider() {
        assert_eq!(
            choose_ramp(true, false, false, true).unwrap(),
            RampChoice::Shadow
        );
    }

    #[test]
    fn ramp_shadow_refuses_to_start_with_a_provider_or_dev_funding() {
        let err = choose_ramp(true, true, false, true).unwrap_err();
        assert!(err
            .to_string()
            .contains("MINT_SHADOW=1 with a payment provider"));
        let err = choose_ramp(true, false, true, true).unwrap_err();
        assert!(err.to_string().contains("mutually exclusive"));
    }

    #[test]
    fn ramp_dev_fake_allowed_in_production_only_with_explicit_opt_in() {
        assert_eq!(
            choose_ramp(true, false, true, false).unwrap(),
            RampChoice::DevFake
        );
    }
}
