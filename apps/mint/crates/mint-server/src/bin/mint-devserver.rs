//! `mint-devserver` — a headless, dependency-free dev/integration server that
//! runs the REAL mint-server router / handlers / auth / ledger over a real TCP
//! socket, backed by the in-memory store and a deterministic auto-confirming
//! funding ramp.
//!
//! It exists so an out-of-process client (e.g. the ReelForge Python wallet's
//! real `httpx` `MintClient`) can drive the genuine HTTP money surface end to
//! end — create account → fund → reserve → capture / release — without a
//! Postgres database or a live payment provider. Every money movement runs
//! through the exact same `build_router` + `AppState` + `Ledger` /
//! `ReserveLedger` code paths the production binary uses; ONLY the storage
//! backend (in-memory) and the funding ramp (auto-confirm) are dev doubles, in
//! the same spirit as the crate's own `tests/api.rs`.
//!
//! This binary is DEV/TEST ONLY. It never touches a database and refuses no
//! deposit, so it must never be deployed. Production runs `mint-server`
//! (main.rs) with `DATABASE_URL` + a real provider.
//!
//! Env:
//! - `MINT_BIND_ADDR` — listen addr (default `127.0.0.1:8080`).
//! - `MINT_API_KEY`   — required `X-API-Key` for all non-health routes
//!                      (default `dev-mint-key` so a bare `cargo run` works).

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use mint_core::Micros;
use mint_protocol::InMemoryPayerKeys;
use mint_receipts::{FixedSigner, Signer};
use mint_reserve::{
    Asset, Deposit, FakeKycGate, KycTier, Payout, PayoutDest, PayoutStatus, Ramp, RampError,
};
use mint_server::{build_router, AppState};
use mint_store::{Clock, InMemoryStore, SystemClock};

/// A deterministic funding ramp that confirms EVERY deposit reference for an
/// effectively unbounded amount. This lets a test client top up any freshly
/// created account via `POST /v1/mint` with an arbitrary `deposit_ref`, so the
/// reserve → capture / release flow can be exercised against real balances.
///
/// The mint is still idempotent per `deposit_ref` inside the ledger, so a given
/// reference funds exactly once — a client uses a fresh reference per top-up.
struct AutoConfirmRamp;

#[async_trait]
impl Ramp for AutoConfirmRamp {
    async fn confirm_deposit(&self, deposit_ref: &str) -> Result<Deposit, RampError> {
        Ok(Deposit {
            deposit_ref: deposit_ref.to_string(),
            // Larger than any realistic single mint; the ledger caps the actual
            // credit at the requested `amount_micros`, so this only removes the
            // "deposit smaller than request" gate for dev funding.
            amount_micros: Micros::new(i64::MAX),
            asset: Asset::Usd,
            confirmed: true,
            bound_account: None,
        })
    }

    async fn initiate_payout(
        &self,
        payout_ref: &str,
        _dest: &PayoutDest,
        amount: Micros,
    ) -> Result<Payout, RampError> {
        Ok(Payout {
            payout_ref: payout_ref.to_string(),
            amount_micros: amount,
            status: PayoutStatus::Paid,
        })
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    init_tracing();

    let bind: SocketAddr = std::env::var("MINT_BIND_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_string())
        .parse()
        .context("MINT_BIND_ADDR must be a valid socket address")?;
    let api_key = std::env::var("MINT_API_KEY").unwrap_or_else(|_| "dev-mint-key".to_string());

    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let signer: Arc<dyn Signer> = Arc::new(FixedSigner::new());
    let store = InMemoryStore::with_clock_and_signer(clock.clone(), signer);

    let ramp: Arc<dyn Ramp> = Arc::new(AutoConfirmRamp);
    // Permissive KYC so a dev redeem is possible; holds/captures do not need it.
    let kyc = Arc::new(FakeKycGate::new(KycTier::Verified, i64::MAX));
    let payer_keys = Arc::new(InMemoryPayerKeys::new());

    let state = AppState::new(store, clock, ramp, kyc, payer_keys, api_key, "whsec_dev");
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    let local = listener.local_addr().context("resolving local addr")?;
    // A machine-readable ready line so a spawning test can wait for the socket
    // then read the actually-bound port (useful when MINT_BIND_ADDR uses :0).
    println!("MINT_DEVSERVER_LISTENING http://{local}");
    tracing::warn!(%local, "mint-devserver (DEV ONLY: in-memory store + auto-confirm ramp)");

    axum::serve(listener, app)
        .await
        .context("axum serve failed")?;
    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer())
        .init();
}
