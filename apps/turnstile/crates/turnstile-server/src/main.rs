//! Reference Turnstile server.
//!
//! Wires the REAL stack — [`ReqwestTransport`](aitp_client::ReqwestTransport) →
//! [`MintVerifier`] → [`Turnstile`] — against a live mint-server, exposing:
//! - `GET  /healthz`  — open liveness probe;
//! - `<method> /v1/tools/:name/call` — every live paid tool, routed through the
//!   runtime-editable registry (see [`live`]) and gated by its own priced gate;
//! - `POST /v1/echo`  — the built-in demo tool (only with `TURNSTILE_DEMO_ECHO=1`);
//! - `GET  /v1/settlements/:request_id` — poll the terminal settlement state and
//!   signed receipt for a paid request by its `AITP-Request-Id`;
//! - `GET  /v1/tools` and `GET /.well-known/aitp-tools` — open discovery of the
//!   live priced-tool catalog (payee, currency, price per tool);
//! - `POST /mcp` — JSON-RPC MCP endpoint exposing the same paid tools, and
//!   `GET /mcp` — the server stream pushing `notifications/tools/list_changed`.
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
//! Paid tools are runtime-editable. They persist in Postgres
//! (`TURNSTILE_DATABASE_URL`, table `turnstile_paid_tools`) or, without a
//! database, in process memory. `TURNSTILE_TOOLS_PATH` is an optional JSON seed
//! applied only while the store is empty. Operators add, reprice, disable and
//! delete tools from the console or `/v1/admin/tools` without a restart; other
//! replicas pick edits up within `TURNSTILE_TOOLS_RELOAD_SECS` (default 5).
//! The server refuses to start only when it could never serve a tool: no seed
//! file, no stored tools, no demo echo and no admin token to add one.
//!
//! x402: set `TURNSTILE_X402_PAY_TO` (and, off testnet, either
//! `TURNSTILE_X402_CDP_API_KEY_ID` + `_SECRET` for the Coinbase CDP facilitator
//! or `TURNSTILE_X402_FACILITATOR_URL`) to also accept x402 USDC payments
//! (v1 `X-PAYMENT`, v2 `PAYMENT-SIGNATURE`, MCP `_meta["x402/payment"]`) on
//! every paid tool; `GET /.well-known/x402` lists them and
//! `/v1/admin/x402/*` reports settlements (see [`turnstile_core::x402`]).
//!
//! Operator access: `TURNSTILE_ADMIN_TOKEN` enables the console at `/admin`,
//! `GET /v1/admin/settlements`, `GET /v1/admin/summary`,
//! `POST /v1/admin/recovery` and the tool editor API (`/v1/admin/tools`,
//! `/v1/admin/tool-audit`). Upstream guardrails for API-edited tools:
//! `TURNSTILE_UPSTREAM_ALLOW_HOSTS`, `TURNSTILE_UPSTREAM_ALLOW_PRIVATE`,
//! `TURNSTILE_UPSTREAM_AUTH_ENVS` (see [`policy`]).

mod admin_tools;
mod live;
mod mcp;
mod operator;
mod policy;
mod tools;
mod x402_admin;

use std::sync::Arc;
use std::time::Duration;

use aitp_client::ReqwestTransport;
use axum::http::StatusCode;
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::{any, get, post};
use axum::Router;
use turnstile_axum::{admit, PaidRequest};
use turnstile_core::{
    spawn_reaper, spawn_settlement_reaper, AmountMicros, InMemoryNonceStore,
    InMemorySettlementJournal, InMemoryToolStore, MintVerifier, NonceStore, PaymentVerifier,
    PostgresNonceStore, PostgresSettlementJournal, PostgresToolStore, SettlementJournal,
    ToolStore, TurnstileConfig, X402Config, X402Gate,
};
use turnstile_core::x402::{
    spawn_x402_credit_reaper, HttpFacilitator, InMemoryX402Journal, MintDepositPoster,
    PostgresX402Journal, X402Journal,
};

use crate::admin_tools::AdminToolsState;
use crate::live::{
    call_tool, discover_live, discover_x402, spawn_reloader, BuiltinEcho, GateFactory, LiveRegistry,
};
use crate::mcp::{mcp_endpoint, mcp_stream, McpState};
use crate::operator::{list_settlements, run_recovery, settlement_for_payer, summary, OperatorState};
use crate::policy::UpstreamPolicy;
use crate::tools::load_specs;
use crate::x402_admin::{X402AdminState, MAX_CREDIT_ATTEMPTS};

/// Default seconds between checks for tool edits made on another replica.
const DEFAULT_RELOAD_SECS: u64 = 5;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = TurnstileConfig::from_env()?;

    // Production wiring: a real reqwest-backed transport to the mint-server.
    let transport = Arc::new(ReqwestTransport::new(config.mint_server_url.clone())?);
    let verifier: Arc<dyn PaymentVerifier> = Arc::new(MintVerifier::new(
        transport,
        config.api_key.clone(),
        config.payee.clone(),
    ));
    // x402 (open HTTP-402 standard) next to AITP-402: off unless
    // TURNSTILE_X402_PAY_TO is set; a half-configured rail refuses to start.
    let x402_config = X402Config::from_env()?;
    let mut _nonce_reaper = None;
    let (nonces, settlements, tool_store, x402_journal): (
        Arc<dyn NonceStore>,
        Arc<dyn SettlementJournal>,
        Arc<dyn ToolStore>,
        Arc<dyn X402Journal>,
    ) = match std::env::var("TURNSTILE_DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => {
            let durable = Arc::new(PostgresNonceStore::connect(&url).await?);
            durable.migrate().await?;
            _nonce_reaper = Some(spawn_reaper(durable.clone(), Duration::from_secs(60)));
            let settlements = Arc::new(PostgresSettlementJournal::from_pool(durable.pool().clone()));
            let tools = Arc::new(PostgresToolStore::from_pool(durable.pool().clone()));
            let x402 = Arc::new(PostgresX402Journal::from_pool(durable.pool().clone()));
            (durable, settlements, tools, x402)
        }
        _ => {
            eprintln!(
                "TURNSTILE_DATABASE_URL is unset; challenges, settlement recovery and tool edits are process-local (single replica only, tool edits are lost on restart)"
            );
            (
                Arc::new(InMemoryNonceStore::new()),
                Arc::new(InMemorySettlementJournal::new()),
                Arc::new(InMemoryToolStore::new()),
                Arc::new(InMemoryX402Journal::new()),
            )
        }
    };
    let x402_gate: Option<Arc<X402Gate>> = match x402_config {
        Some(cfg) => {
            // CDP key → per-request signed JWT; else static header; else keyless.
            let facilitator = HttpFacilitator::from_config(
                Arc::new(ReqwestTransport::new(cfg.facilitator_url.clone())?),
                &cfg,
            )?;
            let mint = cfg.mint_account.clone().map(|account| {
                ReqwestTransport::new(config.mint_server_url.clone()).map(|transport| {
                    Arc::new(MintDepositPoster::new(
                        Arc::new(transport),
                        config.api_key.clone(),
                        account,
                    ))
                })
            });
            let mint = mint.transpose()?;
            if mint.is_none() {
                eprintln!(
                    "x402: TURNSTILE_X402_MINT_ACCOUNT is unset; settled USDC is journaled in Turnstile but not credited into Mint"
                );
            }
            println!(
                "x402 enabled (v1 X-PAYMENT + v2 PAYMENT-SIGNATURE, MCP _meta): network {} payTo {} facilitator {} auth {}",
                cfg.network,
                cfg.pay_to,
                cfg.facilitator_url,
                facilitator.auth().describe()
            );
            Some(Arc::new(X402Gate::new(
                cfg,
                Arc::new(facilitator),
                x402_journal.clone(),
                mint,
            )))
        }
        None => None,
    };
    let _x402_credit_reaper = x402_gate.clone().and_then(|gate| {
        spawn_x402_credit_reaper(gate, Duration::from_secs(30), MAX_CREDIT_ATTEMPTS)
    });
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

    let admin_token = std::env::var("TURNSTILE_ADMIN_TOKEN")
        .ok()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    if admin_token.as_ref().is_some_and(|t| t.len() < 24) {
        return Err("TURNSTILE_ADMIN_TOKEN must be at least 24 characters".into());
    }

    // Optional one-time seed: the file only populates an EMPTY store, so edits
    // made in the console are never overwritten by a restart.
    if let Some(path) = std::env::var("TURNSTILE_TOOLS_PATH")
        .ok()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
    {
        let specs = load_specs(&path)?;
        let seed: Vec<(String, serde_json::Value)> = specs
            .iter()
            .map(|spec| Ok((spec.name.clone(), serde_json::to_value(spec)?)))
            .collect::<Result<_, serde_json::Error>>()?;
        let seeded = tool_store
            .seed_if_empty(&seed, &format!("seed file {path}"))
            .await?;
        if seeded > 0 {
            println!("seeded {seeded} paid tool(s) from {path}");
        } else {
            println!("tool store already populated; {path} not re-applied (edit tools in /admin)");
        }
    }

    // One gate per price point, all sharing the nonce store, Mint verifier and
    // settlement journal (so recovery and replay protection span every tool).
    let factory = GateFactory::new(
        config.clone(),
        nonces.clone(),
        verifier.clone(),
        settlements.clone(),
        settlement_lease,
    )
    .with_x402(x402_gate.clone());
    let echo = if demo_echo_enabled() {
        // Demo mode (explicit TURNSTILE_DEMO_ECHO=1): the built-in echo tool at
        // TURNSTILE_PRICE_MICROS. Never served by accident, so a misconfigured
        // deploy cannot charge real money for an echo.
        eprintln!("TURNSTILE_DEMO_ECHO=1: serving the built-in echo demo tool");
        Some(BuiltinEcho {
            gate: factory.gate(config.price_micros),
        })
    } else {
        None
    };
    let registry = Arc::new(LiveRegistry::new(tool_store.clone(), factory, echo.clone()));
    let snapshot = registry.reload().await?;
    for tool in snapshot.tools.values() {
        println!(
            "paid tool {} -> {} ({} micros)",
            tool.spec.name, tool.spec.upstream_url, tool.spec.price_micros
        );
    }
    if snapshot.tools.is_empty() && echo.is_none() && admin_token.is_none() {
        return Err(
            "no paid tools: set TURNSTILE_TOOLS_PATH to a seed registry, or TURNSTILE_ADMIN_TOKEN to add tools from /admin (or TURNSTILE_DEMO_ECHO=1 for the demo)"
                .into(),
        );
    }
    if snapshot.tools.is_empty() && echo.is_none() {
        eprintln!("no paid tools yet; add one at /admin (Tools tab) or POST /v1/admin/tools");
    }
    let reload_every = std::env::var("TURNSTILE_TOOLS_RELOAD_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_RELOAD_SECS);
    let _tool_reloader = spawn_reloader(registry.clone(), Duration::from_secs(reload_every));

    // Paid HTTP tools: one dynamic route resolves the live tool per request.
    let mut paid_routes: Router = Router::new().route(
        "/v1/tools/:name/call",
        any(call_tool).with_state(registry.clone()),
    );
    if let Some(echo) = &echo {
        // <10 lines to gate a route: attach the admit middleware as a route layer.
        let protected = post(echo_handler).route_layer(from_fn_with_state(echo.gate.clone(), admit));
        paid_routes = paid_routes.route("/v1/echo", protected);
    }

    // Discovery API: an OPEN (free, ungated) live catalog of priced tools an
    // agent can read before it spends anything.
    let discovery_api = Router::new()
        .route("/v1/tools", get(discover_live))
        .route("/.well-known/aitp-tools", get(discover_live))
        .route("/.well-known/x402", get(discover_x402))
        .with_state(registry.clone());

    // MCP API: every paid tool over JSON-RPC, each gated by its own priced gate.
    let mcp_state = Arc::new(McpState {
        registry: registry.clone(),
    });
    let mcp_api = Router::new()
        .route("/mcp", post(mcp_endpoint).get(mcp_stream))
        .with_state(mcp_state);

    // Settlement access: payer-scoped status + operator list/summary.
    let operator_state = OperatorState {
        journal: status_journal,
        admin_token: admin_token.map(Arc::from),
        verifier: verifier.clone(),
        lease: settlement_lease,
    };
    let admin_tools_state = AdminToolsState {
        operator: operator_state.clone(),
        registry: registry.clone(),
        policy: Arc::new(UpstreamPolicy::from_env()),
    };
    let x402_admin_api = Router::new()
        .route("/v1/admin/x402/summary", get(x402_admin::summary))
        .route("/v1/admin/x402/payments", get(x402_admin::payments))
        .route("/v1/admin/x402/credit", post(x402_admin::credit_now))
        .with_state(X402AdminState {
            operator: operator_state.clone(),
            gate: x402_gate.clone(),
        });
    let status_api = Router::new()
        .route("/v1/settlements/:request_id", get(settlement_for_payer))
        .route("/v1/admin/settlements", get(list_settlements))
        .route("/v1/admin/summary", get(summary))
        .route("/v1/admin/recovery", post(run_recovery))
        .with_state(operator_state);
    let tools_admin_api = Router::new()
        .route(
            "/v1/admin/tools",
            get(admin_tools::list_tools).post(admin_tools::create_tool),
        )
        .route(
            "/v1/admin/tools/:name",
            get(admin_tools::get_tool)
                .put(admin_tools::update_tool)
                .delete(admin_tools::delete_tool),
        )
        .route("/v1/admin/tools/:name/enable", post(admin_tools::enable_tool))
        .route("/v1/admin/tools/:name/disable", post(admin_tools::disable_tool))
        .route("/v1/admin/tool-audit", get(admin_tools::tool_audit))
        .with_state(admin_tools_state);

    let app = Router::new()
        .route("/healthz", get(healthz))
        .route("/admin", get(admin_page))
        .merge(paid_routes)
        .merge(discovery_api)
        .merge(mcp_api)
        .merge(status_api)
        .merge(tools_admin_api)
        .merge(x402_admin_api);

    let bind_addr = std::env::var("BIND_ADDR").unwrap_or_else(|_| "0.0.0.0:8101".to_string());
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    println!(
        "turnstile-server listening on {bind_addr} (mint: {})",
        config.mint_server_url
    );
    axum::serve(listener, app).await?;
    Ok(())
}

/// The built-in echo tool is opt-in only.
fn demo_echo_enabled() -> bool {
    matches!(
        std::env::var("TURNSTILE_DEMO_ECHO")
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref(),
        Ok("1") | Ok("true") | Ok("yes")
    )
}

/// Open liveness probe — never gated.
async fn healthz() -> impl IntoResponse {
    (StatusCode::OK, "ok\n")
}

/// `GET /admin` — a self-contained operator console (static HTML + vanilla JS, no
/// build step): the paid-tool editor, revenue summary, recent paid calls and
/// per-request settlement lookup. Every data call carries the operator's
/// `Authorization: Bearer <TURNSTILE_ADMIN_TOKEN>`; the page itself holds no data.
async fn admin_page() -> axum::response::Html<&'static str> {
    axum::response::Html(include_str!("../assets/admin.html"))
}

/// The paid tool. Echoes the body and reports ACTUAL usage (here: one microdollar
/// per request byte, capped at the quoted price) so the middleware captures only
/// what was used — success-only billing.
async fn echo_handler(paid: PaidRequest, body: String) -> impl IntoResponse {
    let quoted = paid.price().get();
    let used = (body.len() as i64).clamp(0, quoted);
    paid.report_actual(AmountMicros::new(used));
    (StatusCode::OK, body)
}
