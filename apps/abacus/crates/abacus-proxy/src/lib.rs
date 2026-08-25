//! # abacus-proxy — the inline enforcement reverse-proxy.
//!
//! `POST /v1/proxy` runs the full hot path: authenticate the ingress key, resolve
//! the agent identity + budget policy, price the MAX possible cost, open a Mint
//! reservation as the reserve gate ([`enforce`]), forward the request verbatim to
//! the real provider ([`abacus_core::transport::UpstreamTransport`]), then settle
//! — capture the EXACT actual on 2xx-with-usage, release otherwise ([`settle`],
//! ADR-0002) — recording a Mint receipt as the system-of-record.
//!
//! The state is built from injectable trait objects, so production wires the REAL
//! `abacus-mint-client` + `abacus-upstream`, while tests wire the `abacus-testkit`
//! fakes.

pub mod auth;
pub mod config;
pub mod enforce;
pub mod handler;
pub mod hold_reaper;
pub mod local_store;
pub mod mem_hold_store;
pub mod pricing;
pub mod settle;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::{middleware, Router};

use abacus_core::identity::BudgetRepo;
use abacus_core::receipt::ReceiptStore;
use abacus_core::transport::{MintTransport, UpstreamTransport};

pub use config::Config;
pub use pricing::PricingTable;

/// Shared server state. Every external dependency is a trait object behind `Arc`,
/// so the same server binary runs with the real clients in prod and the fakes in
/// tests.
#[derive(Clone)]
pub struct AppState {
    /// Mint transport (holds / capture / release / allowances).
    pub mint: Arc<dyn MintTransport>,
    /// Upstream provider forwarder.
    pub upstream: Arc<dyn UpstreamTransport>,
    /// Per-agent budget/policy source.
    pub budgets: Arc<dyn BudgetRepo>,
    /// Receipt system-of-record.
    pub receipts: Arc<dyn ReceiptStore>,
    /// Ingress `X-API-Key` secret agents must present.
    pub ingress_api_key: String,
    /// Per-provider/model pricing table → `est_max`.
    pub pricing: Arc<PricingTable>,
    /// Provider key for pricing-table lookup.
    pub provider: String,
    /// Path requests are forwarded to on the upstream base URL.
    pub upstream_path: String,
}

/// Build the router: the enforced `/v1/proxy` route behind the ingress-key
/// middleware, plus unauthenticated liveness/readiness probes.
pub fn build_router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/v1/proxy", post(handler::proxy))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_key,
        ));

    Router::new()
        .merge(protected)
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state)
}

async fn healthz() -> &'static str {
    "ok"
}

async fn readyz() -> &'static str {
    "ready"
}
