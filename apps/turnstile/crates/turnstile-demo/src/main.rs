//! Reference Turnstile server.
//!
//! Wires the REAL stack — [`ReqwestTransport`](aitp_client::ReqwestTransport) →
//! [`MintVerifier`] → [`Turnstile`] — against a live mint-server, exposing:
//! - `GET  /healthz`  — open liveness probe;
//! - `POST /v1/echo`  — ONE paid tool, gated by `turnstile-axum`;
//! - `GET  /v1/settlements/:request_id` — poll the terminal settlement state and
//!   signed receipt for a paid request by its `AITP-Request-Id`.
//!
//! It proves the full loop: an unpaid call gets a `402` + `AITP-Challenge`; a
//! call bearing a valid `AITP-Authorization` is verified against the mint, the
//! handler runs, and the ACTUAL usage is captured (success-only) with an
//! `AITP-Receipt` attached.
//!
//! Configuration is read from the environment (fail-fast); nothing is hardcoded.
//! Required: `MINT_SERVER_URL`, `TURNSTILE_API_KEY`, `TURNSTILE_PAYEE`,
//! `TURNSTILE_PRICE_MICROS`. Optional: `TURNSTILE_CURRENCY`,
//! `TURNSTILE_CHALLENGE_TTL_MS`, `BIND_ADDR` (default `0.0.0.0:8101`).

use std::sync::Arc;
use std::time::Duration;

use aitp_client::ReqwestTransport;
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use turnstile_axum::{admit, settlement_status, PaidRequest};
use turnstile_core::{
    spawn_reaper, spawn_settlement_reaper, AmountMicros, ChallengeIssuer, InMemoryNonceStore,
    InMemorySettlementJournal, MintVerifier, NonceStore, PostgresNonceStore,
    PostgresSettlementJournal, SettlementJournal, Turnstile, TurnstileConfig,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = TurnstileConfig::from_env()?;

    // Production wiring: a real reqwest-backed transport to the mint-server.
    let transport = Arc::new(ReqwestTransport::new(config.mint_server_url.clone())?);
    let verifier = Arc::new(MintVerifier::new(
        transport,
        config.api_key.clone(),
        config.payee.clone(),
    ));
    let mut _nonce_reaper = None;
    let (nonces, settlements): (Arc<dyn NonceStore>, Arc<dyn SettlementJournal>) =
        match std::env::var("TURNSTILE_DATABASE_URL") {
            Ok(url) if !url.trim().is_empty() => {
                let durable = Arc::new(PostgresNonceStore::connect(&url).await?);
                durable.migrate().await?;
                _nonce_reaper = Some(spawn_reaper(durable.clone(), Duration::from_secs(60)));
                let settlements =
                    Arc::new(PostgresSettlementJournal::from_pool(durable.pool().clone()));
                (durable, settlements)
            }
            _ => {
                eprintln!(
                "TURNSTILE_DATABASE_URL is unset; challenge and settlement recovery are process-local (single replica only)"
            );
                (
                    Arc::new(InMemoryNonceStore::new()),
                    Arc::new(InMemorySettlementJournal::new()),
                )
            }
        };
    // Keep a handle to the journal for the settlement-status API before the gate
    // takes ownership of its clone below.
    let status_journal = settlements.clone();
    let issuer = ChallengeIssuer::new(config.clone(), nonces.clone());
    let settlement_lease = Duration::from_secs(30);
    let _settlement_reaper = spawn_settlement_reaper(
        settlements.clone(),
        verifier.clone(),
        Duration::from_secs(10),
        settlement_lease,
        100,
    );
    let gate = Arc::new(Turnstile::with_journal(
        issuer,
        verifier,
        nonces,
        settlements,
        settlement_lease,
    ));

    // <10 lines to gate a route: attach the admit middleware as a route layer.
    let protected = post(echo).route_layer(from_fn_with_state(gate.clone(), admit));

    // Settlement-status API: a client holding an `AITP-Request-Id` (e.g. from a
    // `503 capture_pending`) can poll the terminal state + signed receipt. Its
    // own sub-router carries the journal handle as state.
    let status_api = Router::new()
        .route("/v1/settlements/:request_id", get(settlement_status))
        .with_state(status_journal);

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/echo", protected)
        .merge(status_api);

    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8101".to_string());
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    println!(
        "turnstile-demo listening on {bind_addr} (mint: {})",
        config.mint_server_url
    );
    axum::serve(listener, app).await?;
    Ok(())
}

/// Open liveness probe — never gated.
async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

/// The paid tool. Echoes the body and reports ACTUAL usage (here: one microdollar
/// per request byte, capped at the quoted price) so the middleware captures only
/// what was used — success-only billing.
async fn echo(paid: PaidRequest, body: String) -> impl IntoResponse {
    let quoted = paid.price().get();
    let used = (body.len() as i64).clamp(0, quoted);
    paid.report_actual(AmountMicros::new(used));
    (StatusCode::OK, body)
}
