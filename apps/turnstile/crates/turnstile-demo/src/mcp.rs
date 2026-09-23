//! A minimal, REAL MCP (Model Context Protocol) endpoint for the reference
//! server, proving Turnstile's "paid MCP tool" half end-to-end.
//!
//! MCP speaks JSON-RPC 2.0 over HTTP (Streamable HTTP transport). This handler
//! implements just enough of the protocol — `initialize`, `tools/list`,
//! `tools/call`, `ping`, and the `notifications/initialized` acknowledgement —
//! to expose ONE paid tool (`echo`) gated by the SAME [`Turnstile`] gate the
//! HTTP `POST /v1/echo` route uses. Payment rides inside the tool arguments
//! (reserved key `_aitp_authorization`); an unpaid call returns a JSON-RPC error
//! carrying an AITP-402 challenge, and a paid call captures actual usage and
//! attaches the signed receipt to the result `_meta`.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::{json, Value};
use turnstile_core::mcp::{
    extract_authorization, receipt_meta, tool_arguments, MCP_PAYMENT_REQUIRED,
};
use turnstile_core::{AmountMicros, McpAdmit, McpPaywall, ToolCatalog};

use crate::tools::UpstreamTool;

/// Stable resource identity for the paid MCP echo tool. The gate challenges and
/// validates against this exact string (distinct from the HTTP route resource).
pub const MCP_ECHO_RESOURCE: &str = "tools/call#echo";

/// The MCP protocol revision this reference server advertises.
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// What runs once a tool call is paid.
#[derive(Clone)]
pub enum McpToolKind {
    /// The built-in demo echo tool.
    Echo,
    /// A config-declared upstream endpoint.
    Upstream(Arc<UpstreamTool>),
}

/// One paid MCP tool: its own paywall (priced gate) plus what it runs.
#[derive(Clone)]
pub struct McpTool {
    pub name: String,
    pub resource: String,
    pub input_schema: Value,
    pub paywall: McpPaywall,
    pub kind: McpToolKind,
}

/// Shared state for the MCP endpoint: every paid tool and the discovery catalog
/// used to price/describe tools in `tools/list`.
#[derive(Clone)]
pub struct McpState {
    pub tools: Vec<McpTool>,
    pub catalog: Arc<ToolCatalog>,
}

/// The input schema of the built-in echo tool.
pub fn echo_schema() -> Value {
    json!({
        "type": "object",
        "properties": { "text": { "type": "string", "description": "Text to echo back." } },
        "required": ["text"],
    })
}

/// `POST /mcp` — a single JSON-RPC 2.0 request/response turn.
pub async fn mcp_endpoint(State(state): State<Arc<McpState>>, Json(req): Json<Value>) -> Response {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(Value::as_str).unwrap_or_default();

    match method {
        "initialize" => rpc_result(id, initialize_result()),
        "ping" => rpc_result(id, json!({})),
        "tools/list" => rpc_result(id, tools_list_result(&state)),
        "tools/call" => handle_tool_call(&state, id, req.get("params")).await,
        // Notifications carry no id and expect no body.
        _ if id.is_none() => StatusCode::ACCEPTED.into_response(),
        _ => rpc_error(id, -32601, "method not found", Some(json!({ "method": method }))),
    }
}

/// The `initialize` result: server identity + declared capabilities.
fn initialize_result() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "turnstile-demo", "version": env!("CARGO_PKG_VERSION") },
        "instructions": "Paid MCP tools. Pay per call with an AITP-402 \
                         authorization in the `_aitp_authorization` argument; an \
                         unpaid call returns a JSON-RPC error whose data carries \
                         the challenge.",
    })
}

/// The `tools/list` result, derived from the discovery catalog. Each MCP tool
/// carries its AITP price/scheme/resource in `_meta.aitp` so agents can see the
/// cost before calling.
fn tools_list_result(state: &McpState) -> Value {
    let catalog = &state.catalog;
    let tools: Vec<Value> = catalog
        .tools
        .iter()
        .filter(|tool| tool.resource.starts_with("tools/call#"))
        .map(|tool| {
            let mut schema = state
                .tools
                .iter()
                .find(|t| t.resource == tool.resource)
                .map(|t| t.input_schema.clone())
                .unwrap_or_else(|| json!({ "type": "object" }));
            // Every paid tool accepts the payment in a reserved argument.
            if let Some(props) = schema
                .as_object_mut()
                .map(|o| o.entry("properties").or_insert_with(|| json!({})))
                .and_then(Value::as_object_mut)
            {
                props.insert(
                    "_aitp_authorization".into(),
                    json!({
                        "type": "string",
                        "description": "AITP-402 authorization header value (payment).",
                    }),
                );
            }
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": schema,
                "_meta": {
                    "aitp": {
                        "resource": tool.resource,
                        "price_micros": tool.price_micros.get(),
                        "currency": catalog.currency,
                        "payee": catalog.payee,
                        "scheme": tool.scheme,
                    },
                },
            })
        })
        .collect();
    json!({ "tools": tools })
}

/// Gate + run a `tools/call`. Unknown tools are an invalid-params error.
async fn handle_tool_call(state: &McpState, id: Option<Value>, params: Option<&Value>) -> Response {
    let params = params.cloned().unwrap_or_else(|| json!({}));
    let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
    let Some(tool) = state.tools.iter().find(|t| t.name == name) else {
        return rpc_error(id, -32602, "unknown tool", Some(json!({ "tool": name })));
    };
    if let McpToolKind::Upstream(upstream) = &tool.kind {
        return handle_upstream_call(tool, upstream, id, &params).await;
    }

    let authorization = extract_authorization(&params);
    match tool
        .paywall
        .gate_call(&tool.resource, authorization.as_deref())
        .await
    {
        // Unpaid or invalid: the paywall already built the JSON-RPC error object
        // (challenge in `data`); return it verbatim.
        McpAdmit::Challenge(error) | McpAdmit::Rejected(error) => rpc_error_value(id, error),
        McpAdmit::Admitted(settled) => {
            let args = tool_arguments(&params);
            let text = args
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();

            // Success-only: bill one microdollar per echoed byte, capped at the
            // quoted price (the hold caps it too). Zero-length echo releases the
            // hold and charges nothing.
            let quoted = tool.paywall.gate().price().get();
            let used = (text.len() as i64).clamp(0, quoted);

            match settled.capture(AmountMicros::new(used)).await {
                Ok(receipt) => {
                    let mut result = json!({
                        "content": [{ "type": "text", "text": text }],
                        "isError": false,
                    });
                    if let Some(receipt) = receipt {
                        result["_meta"] = receipt_meta(&receipt);
                    }
                    rpc_result(id, result)
                }
                // Capture could not be confirmed; recovery will continue it. Do
                // not claim success — surface a settlement-pending error.
                Err(error) => rpc_error(
                    id,
                    MCP_PAYMENT_REQUIRED,
                    "payment settlement pending",
                    Some(json!({ "aitp_settlement": "capture_pending", "aitp_error": error.to_string() })),
                ),
            }
        }
    }
}

/// Gate + proxy a config-declared tool: the call `arguments` (minus the payment)
/// are POSTed to the upstream; a 2xx captures, anything else releases.
async fn handle_upstream_call(
    tool: &McpTool,
    upstream: &UpstreamTool,
    id: Option<Value>,
    params: &Value,
) -> Response {
    let authorization = extract_authorization(params);
    match tool
        .paywall
        .gate_call(&tool.resource, authorization.as_deref())
        .await
    {
        McpAdmit::Challenge(error) | McpAdmit::Rejected(error) => rpc_error_value(id, error),
        McpAdmit::Admitted(settled) => {
            let args = tool_arguments(params);
            let body = serde_json::to_vec(&args).unwrap_or_else(|_| b"{}".to_vec());
            let reply = upstream
                .call(Some("application/json"), axum::body::Bytes::from(body))
                .await;
            let reply = match reply {
                Ok(reply) if reply.status.is_success() => reply,
                Ok(reply) => {
                    let _ = settled.release().await;
                    return rpc_result(
                        id,
                        json!({
                            "content": [{ "type": "text", "text": String::from_utf8_lossy(&reply.body) }],
                            "isError": true,
                            "_meta": { "upstream_status": reply.status.as_u16(), "charged": false },
                        }),
                    );
                }
                Err(error) => {
                    let _ = settled.release().await;
                    return rpc_error(id, -32603, "upstream call failed", Some(json!({ "error": error, "charged": false })));
                }
            };
            let quoted = tool.paywall.gate().price().get();
            let amount = upstream.billable(quoted, reply.usage_micros);
            let text = String::from_utf8_lossy(&reply.body).into_owned();
            match settled.capture(amount).await {
                Ok(receipt) => {
                    let mut result = json!({
                        "content": [{ "type": "text", "text": text }],
                        "isError": false,
                    });
                    if let Ok(structured) = serde_json::from_str::<Value>(&text) {
                        if structured.is_object() {
                            result["structuredContent"] = structured;
                        }
                    }
                    if let Some(receipt) = receipt {
                        result["_meta"] = receipt_meta(&receipt);
                    }
                    rpc_result(id, result)
                }
                Err(error) => rpc_error(
                    id,
                    MCP_PAYMENT_REQUIRED,
                    "payment settlement pending",
                    Some(json!({ "aitp_settlement": "capture_pending", "aitp_error": error.to_string() })),
                ),
            }
        }
    }
}

/// A JSON-RPC 2.0 success response.
fn rpc_result(id: Option<Value>, result: Value) -> Response {
    Json(json!({ "jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "result": result }))
        .into_response()
}

/// A JSON-RPC 2.0 error response built from parts.
fn rpc_error(id: Option<Value>, code: i64, message: &str, data: Option<Value>) -> Response {
    let mut error = json!({ "code": code, "message": message });
    if let Some(data) = data {
        error["data"] = data;
    }
    rpc_error_value(id, error)
}

/// A JSON-RPC 2.0 error response from a pre-built error object.
fn rpc_error_value(id: Option<Value>, error: Value) -> Response {
    Json(json!({ "jsonrpc": "2.0", "id": id.unwrap_or(Value::Null), "error": error }))
        .into_response()
}
