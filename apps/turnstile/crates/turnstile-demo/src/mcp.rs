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

/// Stable resource identity for the paid MCP echo tool. The gate challenges and
/// validates against this exact string (distinct from the HTTP route resource).
pub const MCP_ECHO_RESOURCE: &str = "tools/call#echo";

/// The MCP protocol revision this reference server advertises.
const MCP_PROTOCOL_VERSION: &str = "2025-06-18";

/// Shared state for the MCP endpoint: the paywall (over the shared gate) and the
/// discovery catalog used to price/describe tools in `tools/list`.
#[derive(Clone)]
pub struct McpState {
    pub paywall: McpPaywall,
    pub catalog: Arc<ToolCatalog>,
}

/// `POST /mcp` — a single JSON-RPC 2.0 request/response turn.
pub async fn mcp_endpoint(State(state): State<Arc<McpState>>, Json(req): Json<Value>) -> Response {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(Value::as_str).unwrap_or_default();

    match method {
        "initialize" => rpc_result(id, initialize_result()),
        "ping" => rpc_result(id, json!({})),
        "tools/list" => rpc_result(id, tools_list_result(&state.catalog)),
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
fn tools_list_result(catalog: &ToolCatalog) -> Value {
    let tools: Vec<Value> = catalog
        .tools
        .iter()
        .filter(|tool| tool.resource.starts_with("tools/call#"))
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "text": { "type": "string", "description": "Text to echo back." },
                        "_aitp_authorization": {
                            "type": "string",
                            "description": "AITP-402 authorization header value (payment).",
                        },
                    },
                    "required": ["text"],
                },
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

/// Gate + run a `tools/call`. Only the `echo` tool exists; anything else is an
/// invalid-params error.
async fn handle_tool_call(state: &McpState, id: Option<Value>, params: Option<&Value>) -> Response {
    let params = params.cloned().unwrap_or_else(|| json!({}));
    let tool = params.get("name").and_then(Value::as_str).unwrap_or_default();
    if tool != "echo" {
        return rpc_error(id, -32602, "unknown tool", Some(json!({ "tool": tool })));
    }

    let authorization = extract_authorization(&params);
    match state
        .paywall
        .gate_call(MCP_ECHO_RESOURCE, authorization.as_deref())
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
            let quoted = state.paywall.gate().price().get();
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
