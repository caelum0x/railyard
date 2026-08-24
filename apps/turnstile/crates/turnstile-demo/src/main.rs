//! Reference Turnstile server.
//!
//! Wires the REAL stack — [`ReqwestTransport`](aitp_client::ReqwestTransport) →
//! [`MintVerifier`] → [`Turnstile`] — against a live mint-server, exposing:
//! - `GET  /healthz`  — open liveness probe;
//! - `POST /v1/echo`  — ONE paid tool, gated by `turnstile-axum`.
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

use aitp_client::ReqwestTransport;
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use turnstile_axum::{admit, PaidRequest};
use turnstile_core::{
    AmountMicros, ChallengeIssuer, InMemoryNonceStore, MintVerifier, Turnstile, TurnstileConfig,
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
    let nonces = Arc::new(InMemoryNonceStore::new());
    let issuer = ChallengeIssuer::new(config.clone(), nonces.clone());
    let gate = Arc::new(Turnstile::new(issuer, verifier, nonces));

    // <10 lines to gate a route: attach the admit middleware as a route layer.
    let protected = post(echo).route_layer(from_fn_with_state(gate.clone(), admit));
    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/echo", protected);

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
