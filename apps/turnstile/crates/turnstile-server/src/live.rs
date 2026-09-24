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
    TurnstileConfig,
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
        }
    }

    pub fn config(&self) -> &TurnstileConfig {
        &self.config
    }

    /// A gate quoting `price` on every challenge it issues.
    pub fn gate(&self, price: AmountMicros) -> Arc<Turnstile> {
        let mut priced = self.config.clone();
        priced.price_micros = price;
        Arc::new(Turnstile::with_journal(
            ChallengeIssuer::new(priced, self.nonces.clone()),
            self.verifier.clone(),
            self.nonces.clone(),
            self.settlements.clone(),
            self.lease,
        ))
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
    current: RwLock<Arc<Snapshot>>,
    reload_lock: Mutex<()>,
    changes: watch::Sender<i64>,
}

impl LiveRegistry {
    pub fn new(store: Arc<dyn ToolStore>, factory: GateFactory, echo: Option<BuiltinEcho>) -> Self {
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
        let upstream = Arc::new(UpstreamTool::new(spec.clone())?);
        Ok(Arc::new(LiveTool {
            gate: self.factory.gate(AmountMicros::new(spec.price_micros)),
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

/// `GET /v1/tools` and `GET /.well-known/aitp-tools` — the live priced catalog.
pub async fn discover_live(State(registry): State<Arc<LiveRegistry>>) -> Response {
    Json(registry.snapshot().catalog.as_ref()).into_response()
}
