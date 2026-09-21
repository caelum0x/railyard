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
//! - `MINT_DEV_FUNDING=1` — dev-only: explicitly opt into the in-memory dev
//!   funding ramp when no payment provider is configured. Without it, a
//!   production deployment with no provider refuses to start rather than silently
//!   funding against a fake ramp.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::{bail, Context};
use ed25519_dalek::VerifyingKey;
use mint_payments::{
    HttpTransport, PaymentRegistry, PersonaKycVerifier, ReqwestTransport, StripeIdentityKycVerifier,
};
use mint_protocol::InMemoryPayerKeys;
use mint_receipts::{Ed25519Signer, FixedSigner, Signer};
use mint_reserve::{FakeKycGate, KycGate, KycTier, Ramp, StrictKycGate, VerifiedKycGate};
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
    let kyc: Arc<dyn KycGate> = load_kyc_gate(production, transport)?;
    let payer_keys = Arc::new(load_payer_keys()?);

    match database_url {
        Some(url) => {
            tracing::info!("starting with Postgres store");
            let store = PgStore::connect(&url, signer.clone())
                .await
                .context("connecting to Postgres")?;
            store.run_migrations().await.context("running migrations")?;
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
            let store =
                mint_store::InMemoryStore::with_clock_and_signer(clock.clone(), signer.clone());
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
}

/// Pure funding-ramp selection: a configured provider is always used; the dev
/// fake fallback is refused in production unless explicitly opted into.
fn choose_ramp(
    production: bool,
    has_provider: bool,
    dev_funding: bool,
) -> anyhow::Result<RampChoice> {
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
fn load_kyc_gate(
    production: bool,
    transport: Arc<dyn HttpTransport>,
) -> anyhow::Result<Arc<dyn KycGate>> {
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
            let gate: Arc<dyn KycGate> = match p {
                KycProvider::Persona => Arc::new(VerifiedKycGate::new(
                    PersonaKycVerifier::new(transport, key),
                    min_tier,
                    per_tx_limit,
                )),
                KycProvider::StripeIdentity => Arc::new(VerifiedKycGate::new(
                    StripeIdentityKycVerifier::new(transport, key),
                    min_tier,
                    per_tx_limit,
                )),
            };
            tracing::info!(
                provider = ?p,
                per_tx_limit_micros = per_tx_limit,
                min_tier = ?min_tier,
                "KYC: live provider gate installed (authorizes against provider-verified identity)"
            );
            Ok(gate)
        }
        KycChoice::DevFake => {
            tracing::warn!(
                "MINT_DEV_KYC_ALLOW=1 — installing permissive FakeKycGate (dev only; approves \
                 caller-presented redemptions)"
            );
            Ok(Arc::new(FakeKycGate::new(KycTier::Basic, i64::MAX)))
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
            Ok(Arc::new(StrictKycGate::new()))
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
    match choose_ramp(production, has_provider, dev_funding)? {
        RampChoice::Real => Ok(payments.ramp()),
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
        assert_eq!(choose_ramp(true, true, false).unwrap(), RampChoice::Real);
        assert_eq!(choose_ramp(false, true, true).unwrap(), RampChoice::Real);
    }

    #[test]
    fn ramp_dev_fake_allowed_in_dev_without_provider() {
        assert_eq!(
            choose_ramp(false, false, false).unwrap(),
            RampChoice::DevFake
        );
    }

    #[test]
    fn ramp_dev_fake_refused_in_production_without_opt_in() {
        let err = choose_ramp(true, false, false).unwrap_err();
        assert!(err.to_string().contains("refusing to start"));
    }

    #[test]
    fn ramp_dev_fake_allowed_in_production_only_with_explicit_opt_in() {
        assert_eq!(choose_ramp(true, false, true).unwrap(), RampChoice::DevFake);
    }
}
