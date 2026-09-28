//! # switchyard-mcp
//!
//! The Model Context Protocol message layer, transport-free. The server crate
//! mounts [`handle_http_body`] on `POST /mcp` (streamable HTTP, JSON
//! responses); the CLI's `sy mcp` stdio shim forwards newline-delimited
//! JSON-RPC to the same endpoint.
//!
//! Written against the MCP specification directly (JSON-RPC 2.0; methods
//! `initialize`, `notifications/initialized`, `ping`, `tools/list`,
//! `tools/call`, `resources/list`, `resources/read`). Kept dependency-free
//! instead of pulling `rmcp` so the arm64 binary stays small; the wire format
//! is the same.

use async_trait::async_trait;
use serde::Serialize;
use serde_json::{json, Value};
use switchyard_core::ApiError;

pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
pub const SERVER_NAME: &str = "switchyard";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolDef {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: Value,
    #[serde(rename = "outputSchema", skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    pub annotations: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResourceDef {
    pub uri: String,
    pub name: String,
    pub description: String,
    #[serde(rename = "mimeType")]
    pub mime_type: String,
}

#[async_trait]
pub trait McpBackend: Send + Sync {
    async fn list_tools(&self) -> Result<Vec<ToolDef>, ApiError>;
    /// Run a tool; the value becomes `structuredContent`.
    async fn call_tool(&self, name: &str, args: Value) -> Result<Value, ApiError>;
    async fn list_resources(&self) -> Result<Vec<ResourceDef>, ApiError>;
    /// `(mime_type, text)`.
    async fn read_resource(&self, uri: &str) -> Result<(String, String), ApiError>;
}

fn rpc_error(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut err = json!({ "code": code, "message": message });
    if let Some(d) = data {
        err["data"] = d;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": err })
}

fn rpc_ok(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

/// Handle one JSON-RPC message. Notifications (no `id`) return `None`.
pub async fn handle_message(backend: &dyn McpBackend, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str);
    let params = msg.get("params").cloned().unwrap_or(json!({}));
    let Some(method) = method else {
        return Some(rpc_error(id.unwrap_or(Value::Null), -32600, "invalid request", None));
    };
    let id = match id {
        Some(i) => i,
        None => return None, // notification (e.g. notifications/initialized)
    };
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL_VERSIONS[0]);
            let version = if PROTOCOL_VERSIONS.contains(&asked) { asked } else { PROTOCOL_VERSIONS[0] };
            Ok(json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false }, "resources": { "listChanged": false } },
                "serverInfo": { "name": SERVER_NAME, "title": "Switchyard tool rail", "version": env!("CARGO_PKG_VERSION") },
                "instructions": "Search tools by task with search_tools, read the price and schema with describe_tool, then call_tool. Every response names the provider and the cost in micro-USD. Never ask the user for provider API keys: credentials are injected server-side.",
            }))
        }
        "ping" => Ok(json!({})),
        "tools/list" => match backend.list_tools().await {
            Ok(tools) => Ok(json!({ "tools": tools })),
            Err(e) => Err(e),
        },
        "tools/call" => {
            let Some(name) = params.get("name").and_then(Value::as_str) else {
                return Some(rpc_error(id, -32602, "tools/call needs params.name", None));
            };
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match backend.list_tools().await {
                Ok(tools) if !tools.iter().any(|t| t.name == name) => {
                    return Some(rpc_error(id, -32602, &format!("unknown tool {name}"), None));
                }
                Err(e) => Err(e),
                Ok(_) => Ok(match backend.call_tool(name, args).await {
                    Ok(v) => json!({
                        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }],
                        "structuredContent": v,
                        "isError": false,
                    }),
                    // Tool failures are results the model can read and act on.
                    Err(e) => json!({
                        "content": [{ "type": "text", "text": serde_json::to_string_pretty(&e.body()).unwrap_or_default() }],
                        "structuredContent": e.body(),
                        "isError": true,
                    }),
                }),
            }
        }
        "resources/list" => backend.list_resources().await.map(|r| json!({ "resources": r })),
        "resources/read" => {
            let Some(uri) = params.get("uri").and_then(Value::as_str) else {
                return Some(rpc_error(id, -32602, "resources/read needs params.uri", None));
            };
            backend.read_resource(uri).await.map(|(mime, text)| json!({ "contents": [{ "uri": uri, "mimeType": mime, "text": text }] }))
        }
        "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
        "prompts/list" => Ok(json!({ "prompts": [] })),
        other => return Some(rpc_error(id, -32601, &format!("method not found: {other}"), None)),
    };
    Some(match result {
        Ok(r) => rpc_ok(id, r),
        Err(e) => rpc_error(id, -32000 - (e.status as i64 % 1000), &e.message, Some(e.body())),
    })
}

/// Handle a POST body: one message or a batch array. `None` means "202
/// Accepted, no body" (only notifications).
pub async fn handle_http_body(backend: &dyn McpBackend, body: &Value) -> Option<Value> {
    match body {
        Value::Array(items) => {
            let mut out = Vec::new();
            for m in items {
                if let Some(r) = handle_message(backend, m).await {
                    out.push(r);
                }
            }
            (!out.is_empty()).then(|| Value::Array(out))
        }
        single => handle_message(backend, single).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    #[async_trait]
    impl McpBackend for Echo {
        async fn list_tools(&self) -> Result<Vec<ToolDef>, ApiError> {
            Ok(vec![ToolDef {
                name: "echo".into(),
                title: None,
                description: "echo".into(),
                input_schema: json!({"type": "object"}),
                output_schema: None,
                annotations: json!({"readOnlyHint": true}),
            }])
        }
        async fn call_tool(&self, _name: &str, args: Value) -> Result<Value, ApiError> {
            if args.get("fail").is_some() {
                return Err(ApiError::new(402, "insufficient_balance", "top up"));
            }
            Ok(args)
        }
        async fn list_resources(&self) -> Result<Vec<ResourceDef>, ApiError> {
            Ok(vec![])
        }
        async fn read_resource(&self, uri: &str) -> Result<(String, String), ApiError> {
            Err(ApiError::not_found(uri.to_string()))
        }
    }

    #[tokio::test]
    async fn lifecycle_and_calls() {
        let init = handle_message(&Echo, &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-03-26"}})).await.unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert!(handle_message(&Echo, &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).await.is_none());
        let list = handle_message(&Echo, &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"})).await.unwrap();
        assert_eq!(list["result"]["tools"][0]["inputSchema"]["type"], "object");
        let call = handle_message(&Echo, &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "echo", "arguments": {"a": 1}}})).await.unwrap();
        assert_eq!(call["result"]["structuredContent"]["a"], 1);
        let failed = handle_message(&Echo, &json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "echo", "arguments": {"fail": true}}})).await.unwrap();
        assert_eq!(failed["result"]["isError"], true);
        assert_eq!(failed["result"]["structuredContent"]["error"]["code"], "insufficient_balance");
        let unknown = handle_message(&Echo, &json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call", "params": {"name": "nope"}})).await.unwrap();
        assert_eq!(unknown["error"]["code"], -32602);
        let missing = handle_message(&Echo, &json!({"jsonrpc": "2.0", "id": 6, "method": "bogus"})).await.unwrap();
        assert_eq!(missing["error"]["code"], -32601);
        let res = handle_message(&Echo, &json!({"jsonrpc": "2.0", "id": 7, "method": "resources/read", "params": {"uri": "skill://x"}})).await.unwrap();
        assert!(res["error"].is_object());
    }

    #[tokio::test]
    async fn batches() {
        let out = handle_http_body(&Echo, &json!([{"jsonrpc": "2.0", "id": 1, "method": "ping"}, {"jsonrpc": "2.0", "method": "notifications/initialized"}])).await.unwrap();
        assert_eq!(out.as_array().unwrap().len(), 1);
        assert!(handle_http_body(&Echo, &json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).await.is_none());
    }
}
