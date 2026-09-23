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

pub mod admin;
pub mod audit;
pub mod auth;
pub mod config;
pub mod enforce;
pub mod handler;
pub mod hold_reaper;
pub mod local_hold_store;
pub mod local_store;
pub mod mem_hold_store;
pub mod postgres_hold_store;
pub mod postgres_product_store;
pub mod pricing;
pub mod settle;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::{middleware, Router};

use abacus_core::hold_store::HoldStore;
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
    /// Restart-safe table of Mint holds currently between reserve and terminal.
    pub holds: Arc<dyn HoldStore>,
    /// Ingress `X-API-Key` secret agents must present.
    pub ingress_api_key: String,
    /// Separate operator `X-API-Key` for fleet-wide views
    /// (`ABACUS_ADMIN_API_KEY`). `None` disables the operator API.
    pub admin_api_key: Option<String>,
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
        // An agent's own remaining budget + in-flight reservations.
        .route("/v1/budget", get(admin::own_budget))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_api_key,
        ));

    // Receipt lookup: an agent reading its own spend, or an operator.
    let receipts = Router::new()
        .route("/v1/receipts/:request_id", get(audit::get_receipt))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_ingress_or_admin_key,
        ));

    // Fleet-wide operator views: separate credential.
    let operator = Router::new()
        .route("/v1/admin/overview", get(admin::overview))
        .route("/v1/admin/receipts", get(admin::receipts))
        .route("/v1/admin/holds", get(admin::holds))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_admin_key,
        ));

    Router::new()
        .merge(protected)
        .merge(receipts)
        .merge(operator)
        .route("/admin", get(admin_page))
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .with_state(state)
}

/// `GET /admin` — the self-contained operator console (static HTML + vanilla
/// JS). The page carries no data; every panel calls the operator API with the
/// operator's `X-API-Key` (`ABACUS_ADMIN_API_KEY`), which the agents' ingress
/// key does not satisfy.
async fn admin_page() -> impl axum::response::IntoResponse {
    use axum::http::header;
    (
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            (header::X_FRAME_OPTIONS, "DENY"),
            (
                header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
                 connect-src 'self'; frame-ancestors 'none'; base-uri 'none'",
            ),
        ],
        include_str!("../assets/admin.html"),
    )
}

async fn healthz() -> &'static str {
    "ok"
}

/// Readiness: the hold store and policy source must answer and Mint must be
/// reachable — otherwise this instance could reserve but not settle, so it
/// reports `503` and a blue-green cutover keeps traffic away.
async fn readyz(
    axum::extract::State(state): axum::extract::State<AppState>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    let checks = [
        ("hold_store", state.holds.list().await.err()),
        ("policy_store", state.budgets.list_agents().await.err()),
        ("mint", state.mint.ready().await.err()),
    ];
    let failed: Vec<serde_json::Value> = checks
        .iter()
        .filter_map(|(name, err)| {
            err.as_ref()
                .map(|e| serde_json::json!({ "check": name, "error": e.to_string() }))
        })
        .collect();
    if failed.is_empty() {
        (StatusCode::OK, "ready").into_response()
    } else {
        tracing::warn!(?failed, "abacus not ready");
        (
            StatusCode::SERVICE_UNAVAILABLE,
            axum::Json(serde_json::json!({ "ready": false, "failed": failed })),
        )
            .into_response()
    }
}
