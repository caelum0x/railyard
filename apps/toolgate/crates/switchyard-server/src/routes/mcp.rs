//! `POST /mcp`: MCP over streamable HTTP. Default surface: five meta-tools
//! (small context). A project may pin up to 20 tools, exposed first-class
//! with their real schemas and safety annotations.

use async_trait::async_trait;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use switchyard_auth::scopes;
use switchyard_core::{ApiError, SideEffect};
use switchyard_mcp::{McpBackend, ResourceDef, ToolDef};

use super::account::{balance_value, bundle_skill_text, usage_value};
use super::tools::{describe_value, search_value};
use crate::pipeline::{self, CallOptions, HoldMode};
use crate::principal::{authenticate, Principal};
use crate::state::AppState;

pub struct McpCtx {
    pub st: AppState,
    pub p: Principal,
}

/// MCP tool names are `[a-zA-Z0-9_-]`; catalog ids use dots.
pub fn mcp_name(id: &str) -> String {
    id.replace('.', "_")
}

fn annotations(side: SideEffect, idempotent: bool, title: &str) -> Value {
    match side {
        SideEffect::Read => json!({ "title": title, "readOnlyHint": true, "openWorldHint": true, "idempotentHint": idempotent }),
        SideEffect::Write => json!({ "title": title, "readOnlyHint": false, "destructiveHint": false, "openWorldHint": true, "idempotentHint": idempotent }),
        SideEffect::Send | SideEffect::Spend => json!({ "title": title, "readOnlyHint": false, "destructiveHint": true, "openWorldHint": true, "idempotentHint": false }),
    }
}

fn meta_tools(p: &Principal) -> Vec<ToolDef> {
    let mut v = vec![
        ToolDef {
            name: "search_tools".into(),
            title: Some("Search tools by task".into()),
            description: "Find tools for a job described in plain words (\"verify an email\", \"geocode an address\"). Returns capabilities with candidate tools, prices in micro-USD, and whether they are free or need your own key.".into(),
            input_schema: json!({ "type": "object", "properties": {
                "q": { "type": "string", "description": "The task" },
                "side_effect": { "type": "string", "enum": ["read", "write", "send", "spend"] },
                "max_price": { "type": "integer", "description": "Max price in micro-USD" } }, "required": ["q"] }),
            output_schema: None,
            annotations: json!({ "title": "Search tools", "readOnlyHint": true, "openWorldHint": false }),
        },
        ToolDef {
            name: "describe_tool".into(),
            title: Some("Describe a tool".into()),
            description: "Input and output JSON Schema, price, side effect, example and the fallback chain for a tool or capability id. Spends nothing.".into(),
            input_schema: json!({ "type": "object", "properties": { "id": { "type": "string" } }, "required": ["id"] }),
            output_schema: None,
            annotations: json!({ "title": "Describe tool", "readOnlyHint": true, "openWorldHint": false }),
        },
        ToolDef {
            name: "balance".into(),
            title: Some("Balance and caps".into()),
            description: "Prepaid balance, open holds, spend against the project's caps, and the top-up URL.".into(),
            input_schema: json!({ "type": "object", "properties": {} }),
            output_schema: None,
            annotations: json!({ "title": "Balance", "readOnlyHint": true, "openWorldHint": false }),
        },
        ToolDef {
            name: "usage".into(),
            title: Some("Usage".into()),
            description: "Calls and spend for this project, grouped by tool, provider, key or day.".into(),
            input_schema: json!({ "type": "object", "properties": {
                "from": { "type": "string", "format": "date-time" }, "to": { "type": "string", "format": "date-time" },
                "group_by": { "type": "string", "enum": ["tool", "provider", "key", "day"] } } }),
            output_schema: None,
            annotations: json!({ "title": "Usage", "readOnlyHint": true, "openWorldHint": false }),
        },
    ];
    if scopes::has(&p.scopes, scopes::CALL) {
        v.insert(
            2,
            ToolDef {
                name: "call_tool".into(),
                title: Some("Call a tool".into()),
                description: "Run a tool or capability by id with its input. Capabilities route free-first with disclosed fallback. Pass idempotency_key for anything that writes, sends or spends. The response names the provider and the cost.".into(),
                input_schema: json!({ "type": "object", "properties": {
                    "id": { "type": "string" },
                    "input": { "type": "object" },
                    "options": { "type": "object", "properties": {
                        "provider": { "type": "string" }, "max_cost_micros": { "type": "integer" },
                        "cache": { "type": "string", "enum": ["default", "bypass", "only"] }, "dry_run": { "type": "boolean" } } },
                    "idempotency_key": { "type": "string" } }, "required": ["id", "input"] }),
                output_schema: None,
                annotations: json!({ "title": "Call tool", "readOnlyHint": false, "destructiveHint": true, "openWorldHint": true }),
            },
        );
    }
    v
}

impl McpCtx {
    fn pinned(&self) -> Vec<ToolDef> {
        self.p
            .project
            .pinned_tools
            .iter()
            .take(20)
            .filter_map(|id| {
                let (title, description, side, idem) = match self.st.catalog.tool(id) {
                    Some(t) => (t.title.clone(), format!("{} Price: {} µ$ ({:?}).", t.description, t.price.micros, t.price.basis), t.side_effect, t.idempotent),
                    None => {
                        let c = self.st.catalog.capability(id)?;
                        let side = self.st.catalog.tools_for_capability(id).first().map(|t| t.side_effect).unwrap_or(SideEffect::Read);
                        (c.title.clone(), format!("{} Routed free-first across providers.", c.description), side, false)
                    }
                };
                if !scopes::has(&self.p.scopes, side.required_scope()) || !scopes::has(&self.p.scopes, scopes::CALL) {
                    return None;
                }
                Some(ToolDef {
                    name: mcp_name(id),
                    title: Some(title.clone()),
                    description,
                    input_schema: self.st.catalog.input_schema(id)?,
                    output_schema: self.st.catalog.output_schema(id),
                    annotations: annotations(side, idem, &title),
                })
            })
            .collect()
    }

    async fn run(&self, id: &str, input: Value, options: CallOptions, idem: Option<String>) -> Result<Value, ApiError> {
        let o = pipeline::call(&self.st, &self.p, id, input, options, idem, HoldMode::Own).await?;
        Ok(o.body)
    }
}

#[async_trait]
impl McpBackend for McpCtx {
    async fn list_tools(&self) -> Result<Vec<ToolDef>, ApiError> {
        let mut t = meta_tools(&self.p);
        t.extend(self.pinned());
        Ok(t)
    }

    async fn call_tool(&self, name: &str, args: Value) -> Result<Value, ApiError> {
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string);
        match name {
            "search_tools" => search_value(&self.st, &s("q").unwrap_or_default(), s("side_effect").as_deref(), args.get("max_price").and_then(Value::as_i64), Some(8)).await,
            "describe_tool" => describe_value(&self.st, &self.p, &s("id").unwrap_or_default()).await,
            "balance" => balance_value(&self.st, &self.p).await,
            "usage" => {
                let parse = |k: &str| s(k).and_then(|v| chrono::DateTime::parse_from_rfc3339(&v).ok()).map(|d| d.with_timezone(&chrono::Utc));
                usage_value(&self.st, &self.p, parse("from"), parse("to"), s("group_by").as_deref()).await
            }
            "call_tool" => {
                let id = s("id").ok_or_else(|| ApiError::bad_request("call_tool needs `id`"))?;
                let options: CallOptions = serde_json::from_value(args.get("options").cloned().unwrap_or(json!({}))).map_err(|e| ApiError::bad_request(e.to_string()))?;
                self.run(&id, args.get("input").cloned().unwrap_or(json!({})), options, s("idempotency_key")).await
            }
            other => {
                let id = self.p.project.pinned_tools.iter().find(|id| mcp_name(id) == other).cloned().ok_or_else(|| ApiError::not_found(format!("unknown tool {other}")))?;
                self.run(&id, args, CallOptions::default(), None).await
            }
        }
    }

    async fn list_resources(&self) -> Result<Vec<ResourceDef>, ApiError> {
        let mut out: Vec<ResourceDef> = self
            .st
            .store
            .list_bundles(&self.p.org.id)
            .await
            .map_err(|e| ApiError::internal(e.to_string()))?
            .into_iter()
            .map(|b| ResourceDef { uri: format!("skill://{}", b.id), name: b.name, description: b.description, mime_type: "text/markdown".into() })
            .collect();
        for b in self.st.catalog.bundles() {
            if !out.iter().any(|r| r.uri == format!("skill://{}", b.id)) {
                out.push(ResourceDef { uri: format!("skill://{}", b.id), name: b.name.clone(), description: b.description.clone(), mime_type: "text/markdown".into() });
            }
        }
        Ok(out)
    }

    async fn read_resource(&self, uri: &str) -> Result<(String, String), ApiError> {
        let id = uri.strip_prefix("skill://").ok_or_else(|| ApiError::not_found("only skill:// resources exist"))?;
        Ok(("text/markdown".into(), bundle_skill_text(&self.st, &self.p.org.id, id).await?))
    }
}

pub async fn post(State(st): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    let resource = st.oauth.cfg.mcp_resource();
    let p = match authenticate(&st, &headers, &[resource.as_str()]).await {
        Ok(p) => p,
        Err(e) => {
            let mut r = (StatusCode::from_u16(e.status).unwrap_or(StatusCode::UNAUTHORIZED), Json(e.body())).into_response();
            if e.status == 401 {
                let v = format!("Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\"", st.settings.public_url);
                if let Ok(h) = HeaderValue::from_str(&v) {
                    r.headers_mut().insert("www-authenticate", h);
                }
            }
            return r;
        }
    };
    let msg: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, Json(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("parse error: {e}") } }))).into_response(),
    };
    let ctx = McpCtx { st, p };
    match switchyard_mcp::handle_http_body(&ctx, &msg).await {
        Some(v) => Json(v).into_response(),
        None => StatusCode::ACCEPTED.into_response(),
    }
}

pub async fn not_streaming() -> Response {
    (StatusCode::METHOD_NOT_ALLOWED, [("allow", "POST")], "this server answers MCP requests with JSON over POST; no SSE stream is offered").into_response()
}
