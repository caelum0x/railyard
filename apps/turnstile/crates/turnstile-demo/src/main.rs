//! Reference Turnstile server.
//!
//! Wires the REAL stack — [`ReqwestTransport`](aitp_client::ReqwestTransport) →
//! [`MintVerifier`] → [`Turnstile`] — against a live mint-server, exposing:
//! - `GET  /healthz`  — open liveness probe;
//! - `POST /v1/echo`  — ONE paid HTTP tool, gated by `turnstile-axum`;
//! - `GET  /v1/settlements/:request_id` — poll the terminal settlement state and
//!   signed receipt for a paid request by its `AITP-Request-Id`;
//! - `GET  /v1/tools` and `GET /.well-known/aitp-tools` — open discovery of the
//!   provider's priced-tool catalog (payee, currency, price per tool);
//! - `POST /mcp` — a minimal JSON-RPC MCP endpoint exposing the SAME paid `echo`
//!   tool over the Model Context Protocol, gated by the same Turnstile gate.
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
//!
//! Production mode: set `TURNSTILE_TOOLS_PATH` to a JSON tool registry (see
//! [`tools`]) and every declared upstream endpoint becomes a paid tool over
//! HTTP (`POST /v1/tools/<name>/call`) and MCP, each priced independently. The
//! built-in `echo` tool (priced by `TURNSTILE_PRICE_MICROS`) is served only
//! when no registry is configured.
//!
//! Operator access: `TURNSTILE_ADMIN_TOKEN` enables `GET /v1/admin/settlements`
//! and `GET /v1/admin/summary` (captured revenue, pending captures, failures).

mod mcp;
mod operator;
mod tools;

use std::sync::Arc;
use std::time::Duration;

use aitp_client::{ReqwestTransport, Scheme};
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use turnstile_axum::{admit, discover, PaidRequest};
use turnstile_core::{
    spawn_reaper, spawn_settlement_reaper, AmountMicros, ChallengeIssuer, InMemoryNonceStore,
    InMemorySettlementJournal, McpPaywall, MintVerifier, NonceStore, PostgresNonceStore,
    PostgresSettlementJournal, SettlementJournal, ToolDescriptor, ToolRegistry, Turnstile,
    TurnstileConfig,
};

use crate::mcp::{echo_schema, mcp_endpoint, McpState, McpTool, McpToolKind, MCP_ECHO_RESOURCE};
use crate::operator::{list_settlements, settlement_for_payer, summary, OperatorState};
use crate::tools::{load_specs, proxy_http, UpstreamTool};

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
    // Keep a handle to the journal for the settlement-status/operator APIs.
    let status_journal = settlements.clone();
    let settlement_lease = Duration::from_secs(30);
    let _settlement_reaper = spawn_settlement_reaper(
        settlements.clone(),
        verifier.clone(),
        Duration::from_secs(10),
        settlement_lease,
        100,
    );
    // One gate per price point, all sharing the nonce store, Mint verifier and
    // settlement journal (so recovery and replay protection span every tool).
    let make_gate = |price: AmountMicros| {
        let mut priced = config.clone();
        priced.price_micros = price;
        Arc::new(Turnstile::with_journal(
            ChallengeIssuer::new(priced, nonces.clone()),
            verifier.clone(),
            nonces.clone(),
            settlements.clone(),
            settlement_lease,
        ))
    };

    let mut registry = ToolRegistry::from_config(&config);
    let mut mcp_tools: Vec<McpTool> = Vec::new();
    let mut paid_routes: Router = Router::new();

    match std::env::var("TURNSTILE_TOOLS_PATH") {
        Ok(path) if !path.trim().is_empty() => {
            // Production mode: operator-declared upstream tools.
            let specs = load_specs(path.trim())?;
            for spec in specs {
                let price = AmountMicros::new(spec.price_micros);
                let gate = make_gate(price);
                let upstream = Arc::new(UpstreamTool::new(spec.clone())?);
                if spec.http {
                    registry = registry.with_tool(ToolDescriptor::new(
                        spec.name.clone(),
                        spec.http_resource(),
                        spec.description.clone(),
                        price,
                        Scheme::Hold,
                    ));
                    let route = Router::new()
                        .route(
                            &spec.http_path(),
                            post(proxy_http).route_layer(from_fn_with_state(gate.clone(), admit)),
                        )
                        .with_state(upstream.clone());
                    paid_routes = paid_routes.merge(route);
                }
                if spec.mcp {
                    registry = registry.with_tool(ToolDescriptor::new(
                        spec.name.clone(),
                        spec.mcp_resource(),
                        spec.description.clone(),
                        price,
                        Scheme::Hold,
                    ));
                    mcp_tools.push(McpTool {
                        name: spec.name.clone(),
                        resource: spec.mcp_resource(),
                        input_schema: spec
                            .input_schema
                            .clone()
                            .unwrap_or_else(|| serde_json::json!({ "type": "object" })),
                        paywall: McpPaywall::new(gate.clone()),
                        kind: McpToolKind::Upstream(upstream.clone()),
                    });
                }
                println!("paid tool {} -> {} ({} micros)", spec.name, spec.upstream_url, spec.price_micros);
            }
        }
        _ => {
            // Demo mode: the built-in echo tool at TURNSTILE_PRICE_MICROS.
            let gate = make_gate(config.price_micros);
            // <10 lines to gate a route: attach the admit middleware as a route layer.
            let protected = post(echo).route_layer(from_fn_with_state(gate.clone(), admit));
            paid_routes = paid_routes.route("/v1/echo", protected);
            registry = registry
                .with_tool(ToolDescriptor::priced_by(
                    "echo",
                    "POST /v1/echo",
                    "Echo the request body back, billing one microdollar per byte.",
                    &config,
                ))
                .with_tool(ToolDescriptor::priced_by(
                    "echo",
                    MCP_ECHO_RESOURCE,
                    "Echo the `text` argument back over MCP, billing per byte.",
                    &config,
                ));
            mcp_tools.push(McpTool {
                name: "echo".into(),
                resource: MCP_ECHO_RESOURCE.into(),
                input_schema: echo_schema(),
                paywall: McpPaywall::new(gate),
                kind: McpToolKind::Echo,
            });
        }
    }
    let catalog = Arc::new(registry.catalog());

    // Discovery API: an OPEN (free, ungated) catalog of priced tools an agent
    // can read before it spends anything.
    let discovery_api = Router::new()
        .route("/v1/tools", get(discover))
        .route("/.well-known/aitp-tools", get(discover))
        .with_state(catalog.clone());

    // MCP API: every paid tool over JSON-RPC, each gated by its own priced gate.
    let mcp_state = Arc::new(McpState {
        tools: mcp_tools,
        catalog,
    });
    let mcp_api = Router::new()
        .route("/mcp", post(mcp_endpoint))
        .with_state(mcp_state);

    // Settlement access: payer-scoped status + operator list/summary.
    let admin_token = std::env::var("TURNSTILE_ADMIN_TOKEN")
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    if admin_token.as_ref().is_some_and(|t| t.len() < 24) {
        return Err("TURNSTILE_ADMIN_TOKEN must be at least 24 characters".into());
    }
    let operator_state = OperatorState {
        journal: status_journal,
        admin_token: admin_token.map(Arc::from),
    };
    let status_api = Router::new()
        .route("/v1/settlements/:request_id", get(settlement_for_payer))
        .route("/v1/admin/settlements", get(list_settlements))
        .route("/v1/admin/summary", get(summary))
        .with_state(operator_state);

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/admin", get(admin_page))
        .merge(paid_routes)
        .merge(discovery_api)
        .merge(mcp_api)
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

/// `GET /admin` — a self-contained operator console (static HTML + vanilla JS, no
/// build step): the priced-tool catalog, revenue summary, recent paid calls and
/// per-request settlement lookup. Every data call carries the operator's
/// `Authorization: Bearer <TURNSTILE_ADMIN_TOKEN>`; the page itself holds no data.
async fn admin_page() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("../assets/admin.html"))
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
