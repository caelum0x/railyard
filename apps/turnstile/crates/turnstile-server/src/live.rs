//! The live, runtime-editable paid-tool registry.
//!
//! Tools are persisted in a [`ToolStore`] (Postgres when
//! `TURNSTILE_DATABASE_URL` is set). This module turns the stored rows into an
//! immutable [`Snapshot`] — per-tool priced gates, upstream bindings, the
//! discovery catalog and the MCP tool list — and swaps it atomically whenever
//! the store's revision changes (a local admin write reloads immediately; other
//! replicas pick the change up on their next poll).
//!
//! Requests read ONE snapshot for their whole lifetime, so an in-flight call
//! finishes against the gate and upstream it was admitted with even if the tool
//! is repriced, disabled or deleted meanwhile.
//!
//! Pricing semantics on edit: a `402` challenge binds the price it quoted (the
//! nonce store persists the exact challenge), so an authorization for a
//! challenge issued before a reprice is honored at the ORIGINAL price and the
//! capture ceiling is that authorized amount. Disabling or deleting a tool
//! removes its route immediately; outstanding challenges for it can no longer
//! be redeemed (no payment is taken) and simply expire. Holds already admitted
//! settle normally, and the settlement journal plus recovery keep working for
//! the retired resource.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use aitp_client::Scheme;
use axum::extract::{Path, Request, State};
use axum::http::{header, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use tokio::sync::{watch, Mutex};
use turnstile_axum::gate_request;
use turnstile_core::{
    AmountMicros, ChallengeIssuer, McpPaywall, NonceStore, PaymentVerifier, SettlementJournal,
    StoredTool, ToolCatalog, ToolDescriptor, ToolRegistry, ToolStore, ToolStoreError, Turnstile,
    TurnstileConfig, X402Gate,
};
use turnstile_core::x402::{
    PaymentRequiredV2, PaymentRequirements, X402_VERSION, X402_VERSION_V2,
};

use crate::mcp::{echo_schema, McpTool, McpToolKind, MCP_ECHO_RESOURCE};
use crate::tools::{proxy_request, ToolSpec, UpstreamTool};

/// Name reserved by the built-in demo echo tool when it is enabled.
pub const ECHO_TOOL: &str = "echo";

/// Builds priced gates that share one nonce store, Mint verifier and
/// settlement journal, so replay protection and recovery span every tool.
pub struct GateFactory {
    config: TurnstileConfig,
    nonces: Arc<dyn NonceStore>,
    verifier: Arc<dyn PaymentVerifier>,
    settlements: Arc<dyn SettlementJournal>,
    lease: Duration,
    x402: Option<Arc<X402Gate>>,
}

impl GateFactory {
    pub fn new(
        config: TurnstileConfig,
        nonces: Arc<dyn NonceStore>,
        verifier: Arc<dyn PaymentVerifier>,
        settlements: Arc<dyn SettlementJournal>,
        lease: Duration,
    ) -> Self {
        Self {
            config,
            nonces,
            verifier,
            settlements,
            lease,
            x402: None,
        }
    }

    /// Every gate this factory builds also speaks x402 through `gate`.
    #[must_use]
    pub fn with_x402(mut self, gate: Option<Arc<X402Gate>>) -> Self {
        self.x402 = gate;
        self
    }

    pub fn x402(&self) -> Option<&Arc<X402Gate>> {
        self.x402.as_ref()
    }

    pub fn config(&self) -> &TurnstileConfig {
        &self.config
    }

    /// A gate quoting `price` on every challenge it issues.
    pub fn gate(&self, price: AmountMicros) -> Arc<Turnstile> {
        self.gate_described(price, "Paid API call")
    }

    /// A priced gate whose x402 requirements carry `description`.
    pub fn gate_described(&self, price: AmountMicros, description: &str) -> Arc<Turnstile> {
        let mut priced = self.config.clone();
        priced.price_micros = price;
        let gate = Turnstile::with_journal(
            ChallengeIssuer::new(priced, self.nonces.clone()),
            self.verifier.clone(),
            self.nonces.clone(),
            self.settlements.clone(),
            self.lease,
        );
        Arc::new(match &self.x402 {
            Some(x402) => gate.with_x402(x402.clone(), description),
            None => gate,
        })
    }
}

/// One routable paid tool.
pub struct LiveTool {
    pub spec: ToolSpec,
    pub version: i64,
    pub gate: Arc<Turnstile>,
    pub upstream: Arc<UpstreamTool>,
    pub method: Method,
}

/// The built-in demo echo tool (served only with `TURNSTILE_DEMO_ECHO=1`).
#[derive(Clone)]
pub struct BuiltinEcho {
    pub gate: Arc<Turnstile>,
}

/// An immutable view of every routable tool at one store revision.
pub struct Snapshot {
    pub revision: i64,
    /// Enabled tools exposed over HTTP, by name.
    pub http: HashMap<String, Arc<LiveTool>>,
    /// Every enabled tool (HTTP and/or MCP), by name.
    pub tools: BTreeMap<String, Arc<LiveTool>>,
    /// Enabled MCP tools (including the built-in echo in demo mode).
    pub mcp: Vec<McpTool>,
    pub catalog: Arc<ToolCatalog>,
    /// Stored tools that could not be activated (bad spec, missing credential,
    /// reserved name), with the reason. Shown in the console.
    pub errors: BTreeMap<String, String>,
}

pub struct LiveRegistry {
    store: Arc<dyn ToolStore>,
    factory: GateFactory,
    echo: Option<BuiltinEcho>,
    /// Upstream SSRF policy, threaded into every [`UpstreamTool`] so it is
    /// re-checked on every real call, not just once when the tool's spec was
    /// last written (see `UpstreamTool`'s own `policy` field doc).
    policy: Arc<crate::policy::UpstreamPolicy>,
    current: RwLock<Arc<Snapshot>>,
    reload_lock: Mutex<()>,
    changes: watch::Sender<i64>,
}

impl LiveRegistry {
    pub fn new(
        store: Arc<dyn ToolStore>,
        factory: GateFactory,
        echo: Option<BuiltinEcho>,
        policy: Arc<crate::policy::UpstreamPolicy>,
    ) -> Self {
        let empty = Snapshot {
            revision: -1,
            http: HashMap::new(),
            tools: BTreeMap::new(),
            mcp: Vec::new(),
            catalog: Arc::new(ToolRegistry::from_config(factory.config()).catalog()),
            errors: BTreeMap::new(),
        };
        let (changes, _) = watch::channel(-1);
        Self {
            store,
            factory,
            echo,
            policy,
            current: RwLock::new(Arc::new(empty)),
            reload_lock: Mutex::new(()),
            changes,
        }
    }

    pub fn store(&self) -> &Arc<dyn ToolStore> {
        &self.store
    }

    pub fn echo_enabled(&self) -> bool {
        self.echo.is_some()
    }

    /// The built-in demo echo tool's gate, when `TURNSTILE_DEMO_ECHO=1` — not
    /// routable through `snapshot.http` (it is served on its own `/v1/echo`
    /// handler, not the generic `/v1/tools/:name/call` dispatcher), so x402
    /// discovery needs this to list it alongside the registry's tools.
    pub fn echo_gate(&self) -> Option<Arc<Turnstile>> {
        self.echo.as_ref().map(|echo| echo.gate.clone())
    }

    /// The snapshot every request reads from.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.current
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_else(|poisoned| poisoned.into_inner().clone())
    }

    /// Subscribe to tool-set changes (the new revision after each swap).
    pub fn subscribe(&self) -> watch::Receiver<i64> {
        self.changes.subscribe()
    }

    /// Rebuild the snapshot from the store and swap it in.
    pub async fn reload(&self) -> Result<Arc<Snapshot>, ToolStoreError> {
        let _serial = self.reload_lock.lock().await;
        let revision = self.store.revision().await?;
        let rows = self.store.list().await?;
        let previous = self.snapshot();
        let next = Arc::new(self.build(revision, &rows, &previous));
        for (name, reason) in &next.errors {
            if previous.errors.get(name) != Some(reason) {
                eprintln!("paid tool {name} is inactive: {reason}");
            }
        }
        match self.current.write() {
            Ok(mut guard) => *guard = next.clone(),
            Err(poisoned) => *poisoned.into_inner() = next.clone(),
        }
        let changed = previous.revision != next.revision;
        if changed {
            self.changes.send_replace(next.revision);
        }
        Ok(next)
    }

    /// Reload only when another replica (or this one) changed the store.
    pub async fn reload_if_changed(&self) -> Result<bool, ToolStoreError> {
        let revision = self.store.revision().await?;
        if revision == self.snapshot().revision {
            return Ok(false);
        }
        self.reload().await.map(|_| true)
    }

    fn activate(&self, row: &StoredTool, previous: &Snapshot) -> Result<Arc<LiveTool>, String> {
        if let Some(existing) = previous.tools.get(&row.name) {
            // Unchanged row: keep its gate and pooled upstream client.
            if existing.version == row.version {
                return Ok(existing.clone());
            }
        }
        if self.echo.is_some() && row.name == ECHO_TOOL {
            return Err("the name `echo` is reserved by the built-in demo tool (TURNSTILE_DEMO_ECHO=1)".into());
        }
        let spec: ToolSpec = serde_json::from_value(row.spec.clone())
            .map_err(|e| format!("stored spec is invalid: {e}"))?;
        if spec.name != row.name {
            return Err(format!("stored spec name {:?} does not match row", spec.name));
        }
        spec.validate()?;
        let method: Method = spec
            .http_method
            .parse()
            .map_err(|_| "invalid http_method".to_string())?;
        let upstream = Arc::new(UpstreamTool::new(spec.clone(), self.policy.clone())?);
        Ok(Arc::new(LiveTool {
            gate: self
                .factory
                .gate_described(AmountMicros::new(spec.price_micros), &spec.description),
            spec,
            version: row.version,
            upstream,
            method,
        }))
    }

    fn build(&self, revision: i64, rows: &[StoredTool], previous: &Snapshot) -> Snapshot {
        let mut registry = ToolRegistry::from_config(self.factory.config());
        let mut http = HashMap::new();
        let mut tools = BTreeMap::new();
        let mut mcp = Vec::new();
        let mut errors = BTreeMap::new();

        if let Some(echo) = &self.echo {
            let config = self.factory.config();
            registry = registry
                .with_tool(ToolDescriptor::priced_by(
                    ECHO_TOOL,
                    "POST /v1/echo",
                    "Echo the request body back, billing one microdollar per byte.",
                    config,
                ))
                .with_tool(ToolDescriptor::priced_by(
                    ECHO_TOOL,
                    MCP_ECHO_RESOURCE,
                    "Echo the `text` argument back over MCP, billing per byte.",
                    config,
                ));
            mcp.push(McpTool {
                name: ECHO_TOOL.into(),
                resource: MCP_ECHO_RESOURCE.into(),
                input_schema: echo_schema(),
                paywall: McpPaywall::new(echo.gate.clone()),
                kind: McpToolKind::Echo,
            });
        }

        for row in rows.iter().filter(|row| row.enabled && !row.deleted) {
            let tool = match self.activate(row, previous) {
                Ok(tool) => tool,
                Err(reason) => {
                    errors.insert(row.name.clone(), reason);
                    continue;
                }
            };
            let spec = &tool.spec;
            let price = AmountMicros::new(spec.price_micros);
            if spec.http {
                registry = registry.with_tool(ToolDescriptor::new(
                    spec.name.clone(),
                    spec.http_resource(),
                    spec.description.clone(),
                    price,
                    Scheme::Hold,
                ));
                http.insert(spec.name.clone(), tool.clone());
            }
            if spec.mcp {
                registry = registry.with_tool(ToolDescriptor::new(
                    spec.name.clone(),
                    spec.mcp_resource(),
                    spec.description.clone(),
                    price,
                    Scheme::Hold,
                ));
                mcp.push(McpTool {
                    name: spec.name.clone(),
                    resource: spec.mcp_resource(),
                    input_schema: spec
                        .input_schema
                        .clone()
                        .unwrap_or_else(|| json!({ "type": "object" })),
                    paywall: McpPaywall::new(tool.gate.clone()),
                    kind: McpToolKind::Upstream(tool.upstream.clone()),
                });
            }
            tools.insert(spec.name.clone(), tool);
        }

        Snapshot {
            revision,
            http,
            tools,
            mcp,
            catalog: Arc::new(registry.catalog()),
            errors,
        }
    }
}

/// Poll the store so edits made on another replica are picked up.
pub fn spawn_reloader(registry: Arc<LiveRegistry>, every: Duration) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(every);
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if let Err(e) = registry.reload_if_changed().await {
                eprintln!("paid tool reload failed (serving the previous tool set): {e}");
            }
        }
    })
}

/// `<method> /v1/tools/:name/call` — route to the live tool, run its gate,
/// then proxy upstream (success-only capture).
pub async fn call_tool(
    State(registry): State<Arc<LiveRegistry>>,
    Path(name): Path<String>,
    req: Request,
) -> Response {
    let snapshot = registry.snapshot();
    let Some(tool) = snapshot.http.get(&name).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "unknown_tool", "message": format!("no paid tool named {name:?}") })),
        )
            .into_response();
    };
    drop(snapshot);
    if req.method() != tool.method {
        let mut response = (
            StatusCode::METHOD_NOT_ALLOWED,
            Json(json!({
                "error": "method_not_allowed",
                "message": format!("{name} is called with {}", tool.method),
            })),
        )
            .into_response();
        if let Ok(allow) = tool.method.as_str().parse() {
            response.headers_mut().insert(header::ALLOW, allow);
        }
        return response;
    }
    let upstream = tool.upstream.clone();
    gate_request(&tool.gate, req, move |req| proxy_request(upstream, req)).await
}

/// `GET /.well-known/x402` — x402 discovery: every live HTTP tool as x402 v1
/// `PaymentRequirements` (the same objects its `402` advertises) and every MCP
/// tool as a v2 `PaymentRequired` (`mcp://tool/<name>`), so x402 clients and
/// directories can list the tools before paying.
pub async fn discover_x402(State(registry): State<Arc<LiveRegistry>>) -> Response {
    let snapshot = registry.snapshot();
    let mut accepts: Vec<PaymentRequirements> = snapshot
        .http
        .values()
        .filter_map(|tool| tool.gate.x402_requirements(&tool.spec.http_path()))
        .collect();
    // The built-in demo echo tool (`TURNSTILE_DEMO_ECHO=1`) is served on its
    // own `/v1/echo` route, not through `snapshot.http`'s generic dispatcher,
    // so it is invisible to the loop above even though its own `402` already
    // advertises an x402 `PaymentRequirements` (checked separately below for
    // MCP via `snapshot.mcp`, which already includes it).
    if let Some(gate) = registry.echo_gate() {
        accepts.extend(gate.x402_requirements("/v1/echo"));
    }
    accepts.sort_by(|a, b| a.resource.cmp(&b.resource));
    let mcp: Vec<PaymentRequiredV2> = snapshot
        .mcp
        .iter()
        .filter_map(|tool| tool.paywall.x402_requirements(&tool.name))
        .filter_map(|r| PaymentRequiredV2::from_v1(&X402Gate::required_body(vec![r], "")))
        .collect();
    match registry.factory.x402() {
        Some(x402) => Json(json!({
            "x402Version": X402_VERSION,
            "x402Versions": [X402_VERSION, X402_VERSION_V2],
            "network": x402.config().network,
            "payTo": x402.config().pay_to,
            "asset": x402.config().asset,
            "facilitator": x402.config().facilitator_url,
            "accepts": accepts,
            "mcp": mcp,
        }))
        .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({
                "error": "x402_disabled",
                "message": "this Turnstile does not accept x402 payments (set TURNSTILE_X402_PAY_TO)",
            })),
        )
            .into_response(),
    }
}

/// `GET /v1/tools` and `GET /.well-known/aitp-tools` — the live priced catalog.
pub async fn discover_live(State(registry): State<Arc<LiveRegistry>>) -> Response {
    Json(registry.snapshot().catalog.as_ref()).into_response()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use turnstile_core::x402::config::USDC_BASE_SEPOLIA;
    use turnstile_core::x402::{InMemoryX402Journal, X402Config};
    use turnstile_core::{
        FakeVerifier, InMemoryNonceStore, InMemorySettlementJournal, InMemoryToolStore,
        TurnstileConfig,
    };

    use super::*;

    /// A registry with `TURNSTILE_DEMO_ECHO=1` and x402 enabled, same shape
    /// `main.rs` builds in production.
    async fn registry_with_echo_and_x402() -> Arc<LiveRegistry> {
        let config = TurnstileConfig {
            mint_server_url: "http://mint.internal".into(),
            api_key: "k".into(),
            payee: "ait1qpayee".into(),
            price_micros: AmountMicros(1_000),
            currency: "AIT".into(),
            challenge_ttl_ms: 60_000,
        };
        let x402_facilitator: Arc<dyn turnstile_core::x402::Facilitator> =
            Arc::new(turnstile_core::x402::HttpFacilitator::new(
                Arc::new(
                    aitp_client::transport::ReqwestTransport::new("https://x402.org/facilitator")
                        .unwrap(),
                ),
                None,
            ));
        let x402 = Arc::new(X402Gate::new(
            X402Config {
                pay_to: "0xfBb908AA04C3644364D3998d8f683619e25E7C17".into(),
                network: "base-sepolia".into(),
                facilitator_url: "https://x402.org/facilitator".into(),
                facilitator_auth: None,
                cdp_key: None,
                asset: USDC_BASE_SEPOLIA.into(),
                asset_name: "USD Coin".into(),
                asset_version: "2".into(),
                max_timeout_seconds: 60,
                mint_account: None,
                public_url: Some("https://turnstile.ereboslab.com".into()),
            },
            x402_facilitator,
            Arc::new(InMemoryX402Journal::new()),
            None,
        ));
        let factory = GateFactory::new(
            config.clone(),
            Arc::new(InMemoryNonceStore::new()),
            Arc::new(FakeVerifier::with_hold("hold_1")),
            Arc::new(InMemorySettlementJournal::new()),
            Duration::from_secs(30),
        )
        .with_x402(Some(x402));
        let echo = BuiltinEcho {
            gate: factory.gate_described(config.price_micros, "Echo"),
        };
        let registry = Arc::new(LiveRegistry::new(
            Arc::new(InMemoryToolStore::new()),
            factory,
            Some(echo),
            Arc::new(crate::policy::UpstreamPolicy::default()),
        ));
        registry.reload().await.unwrap();
        registry
    }

    /// Regression test for the bug found 2026-10-29: the built-in demo echo
    /// tool's own `402` advertises a real x402 `PaymentRequirements` (it is
    /// `TURNSTILE_DEMO_ECHO=1`'s only paid route), but `/.well-known/x402`'s
    /// HTTP `accepts` array only ever read `snapshot.http`, which the echo
    /// tool is never inserted into (it is served on its own `/v1/echo`
    /// handler, not the generic `/v1/tools/:name/call` dispatcher) — so an
    /// x402 client doing discovery-first would never find it over HTTP, only
    /// over MCP. `discover_x402` must list it in both places.
    #[tokio::test]
    async fn discover_x402_lists_the_builtin_echo_tool_over_http_too() {
        let registry = registry_with_echo_and_x402().await;
        let resp = discover_x402(State(registry)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

        let accepts = body["accepts"].as_array().expect("accepts array");
        assert_eq!(accepts.len(), 1, "echo tool missing from HTTP accepts: {body}");
        assert_eq!(accepts[0]["resource"], "https://turnstile.ereboslab.com/v1/echo");
        assert_eq!(accepts[0]["payTo"], "0xfBb908AA04C3644364D3998d8f683619e25E7C17");
        assert_eq!(accepts[0]["maxAmountRequired"], "1000");

        // Still listed over MCP too (this half already worked).
        let mcp = body["mcp"].as_array().expect("mcp array");
        assert_eq!(mcp.len(), 1);
        assert_eq!(mcp[0]["resource"]["url"], "mcp://tool/echo");
    }

    #[tokio::test]
    async fn discover_x402_is_404_when_x402_is_disabled() {
        let config = TurnstileConfig {
            mint_server_url: "http://mint.internal".into(),
            api_key: "k".into(),
            payee: "ait1qpayee".into(),
            price_micros: AmountMicros(1_000),
            currency: "AIT".into(),
            challenge_ttl_ms: 60_000,
        };
        let factory = GateFactory::new(
            config.clone(),
            Arc::new(InMemoryNonceStore::new()),
            Arc::new(FakeVerifier::with_hold("hold_1")),
            Arc::new(InMemorySettlementJournal::new()),
            Duration::from_secs(30),
        );
        let echo = BuiltinEcho {
            gate: factory.gate_described(config.price_micros, "Echo"),
        };
        let registry = Arc::new(LiveRegistry::new(
            Arc::new(InMemoryToolStore::new()),
            factory,
            Some(echo),
            Arc::new(crate::policy::UpstreamPolicy::default()),
        ));
        registry.reload().await.unwrap();
        let resp = discover_x402(State(registry)).await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }
}
