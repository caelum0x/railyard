//! Health, readiness, `llms.txt` and `openapi.json`.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::state::AppState;

pub async fn healthz() -> Response {
    Json(json!({ "ok": true })).into_response()
}

pub async fn readyz(State(st): State<AppState>) -> Response {
    match st.store.list_orgs().await {
        Ok(_) => Json(json!({ "ok": true, "tools": st.catalog.tools().count(), "mint": st.mint.is_some(), "rago": st.rago.is_some() })).into_response(),
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, Json(json!({ "ok": false, "error": e.to_string() }))).into_response(),
    }
}

pub async fn llms_txt(State(st): State<AppState>) -> Response {
    let base = &st.settings.public_url;
    let mut caps = String::new();
    for c in st.catalog.capabilities() {
        let tools: Vec<String> = st.catalog.tools_for_capability(&c.id).iter().map(|t| format!("{} ({} µ$ {:?})", t.id, t.price.micros, t.price.basis)).collect();
        caps.push_str(&format!("- `{}`: {} Tools: {}\n", c.id, c.description, tools.join(", ")));
    }
    let text = format!(
        "# Switchyard

> The tool rail next to Relay's model rail. One `sk-t0-` key (or an OAuth token) reaches a schema-typed catalog of tools: our own products first, then free and public routes, then your own keys. Every call names its provider and its cost in micro-USD; failures are never billed.

## Use it

- Base URL: {base}
- Auth: `Authorization: Bearer sk-t0-...` (the same key works on Relay's `/openai/v1`), or OAuth 2.1 (`{base}/.well-known/oauth-authorization-server`).
- MCP: streamable HTTP at `{base}/mcp` (meta-tools `search_tools`, `describe_tool`, `call_tool`, `balance`, `usage`).
- CLI: `sy login`, `sy search \"verify an email\"`, `sy call rally.email.verify --input '{{\"email\":\"a@b.com\"}}'`.
- Never ask a user for provider API keys. Credentials are stored write-only and injected server-side (`sy cred add <provider> --value-stdin`).

## Endpoints

- `GET /v1/tools/search?q=` search by task; `GET /v1/tools/{{id}}` schemas, price and fallback chain (spends nothing).
- `POST /v1/tools/{{id}}/call` body `{{\"input\":{{}},\"options\":{{\"provider\",\"max_cost_micros\",\"cache\",\"dry_run\"}}}}`; send `Idempotency-Key` on writes and sends. `{{id}}` may be a capability (routed, with fallback) or a concrete tool.
- `POST /v1/tools/batch` up to 50 calls under one hold.
- `GET /v1/balance`, `POST /v1/balance/topup`, `GET /v1/usage`, `GET /v1/usage/calls/{{id}}` (receipt and hash-chain proof).
- Errors: 402 balance (`topup_url`), 429 `cap_exceeded` (names the cap), 422 schema (JSON Pointers, nothing spent), 409 idempotency conflict, 424 every provider failed (nothing billed).

## Capabilities

{caps}"
    );
    ([("content-type", "text/plain; charset=utf-8")], text).into_response()
}

pub async fn openapi(State(st): State<AppState>) -> Response {
    let bearer = json!([{ "bearer": [] }]);
    let env = |data: serde_json::Value| json!({ "type": "object", "properties": { "ok": { "type": "boolean" }, "data": data } });
    let err = json!({ "$ref": "#/components/schemas/Error" });
    let doc = json!({
        "openapi": "3.1.0",
        "info": { "title": "Switchyard", "version": env!("CARGO_PKG_VERSION"), "description": "The tool rail: search, describe and call schema-typed tools on one key." },
        "servers": [{ "url": st.settings.public_url }],
        "components": {
            "securitySchemes": { "bearer": { "type": "http", "scheme": "bearer" } },
            "schemas": {
                "Error": { "type": "object", "properties": { "ok": { "const": false }, "error": { "type": "object", "properties": { "code": { "type": "string" }, "message": { "type": "string" }, "details": {} } } } },
                "CallBody": { "type": "object", "properties": { "input": { "type": "object" }, "options": { "type": "object", "properties": {
                    "provider": { "type": "string" }, "max_cost_micros": { "type": "integer" }, "cache": { "enum": ["default", "bypass", "only"] }, "dry_run": { "type": "boolean" } } } } },
            }
        },
        "security": bearer,
        "paths": {
            "/v1/tools/search": { "get": { "summary": "Search tools by task", "parameters": [
                { "name": "q", "in": "query", "required": true, "schema": { "type": "string" } },
                { "name": "side_effect", "in": "query", "schema": { "enum": ["read", "write", "send", "spend"] } },
                { "name": "max_price_micros", "in": "query", "schema": { "type": "integer" } },
                { "name": "limit", "in": "query", "schema": { "type": "integer" } }],
                "responses": { "200": { "description": "hits", "content": { "application/json": { "schema": env(json!({})) } } } } } },
            "/v1/tools/{id}": { "get": { "summary": "Describe a tool or capability (spends nothing)", "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }], "responses": { "200": { "description": "schemas, price, routing" }, "404": { "description": "unknown", "content": { "application/json": { "schema": err } } } } } },
            "/v1/tools/{id}/call": { "post": { "summary": "Call a tool or capability",
                "parameters": [{ "name": "id", "in": "path", "required": true, "schema": { "type": "string" } }, { "name": "Idempotency-Key", "in": "header", "schema": { "type": "string" } }],
                "requestBody": { "content": { "application/json": { "schema": { "$ref": "#/components/schemas/CallBody" } } } },
                "responses": { "200": { "description": "data + meta{call_id, provider, cost_micros, cache, fallback_chain, receipt_id}" },
                    "402": { "description": "insufficient balance" }, "409": { "description": "idempotency conflict" }, "422": { "description": "invalid input" },
                    "424": { "description": "all providers failed" }, "429": { "description": "cap exceeded" } } } },
            "/v1/tools/batch": { "post": { "summary": "Up to 50 calls under one hold" } },
            "/v1/passthrough/{tool_id}/{path}": { "post": { "summary": "Faithful relay for admin-registered BYO tools" } },
            "/v1/keys": { "get": { "summary": "List keys" }, "post": { "summary": "Create a key (one key for Relay and Switchyard)" } },
            "/v1/keys/{id}/revoke": { "post": { "summary": "Revoke a key" } },
            "/v1/keys/{id}": { "patch": { "summary": "Update scopes, tool allowlist, cap" } },
            "/v1/credentials": { "get": { "summary": "List credential metadata (values never returned)" }, "post": { "summary": "Store a credential (write-only)" } },
            "/v1/credentials/{id}": { "delete": { "summary": "Delete a credential" } },
            "/v1/bundles": { "get": { "summary": "List SKILL.md bundles" }, "post": { "summary": "Push a bundle" } },
            "/v1/bundles/{id}/skill.md": { "get": { "summary": "Download SKILL.md (no secrets)" } },
            "/v1/project": { "get": { "summary": "Project settings" }, "patch": { "summary": "Caps, routing policy, pinned MCP tools" } },
            "/v1/usage": { "get": { "summary": "Usage and spend grouped by tool, provider, key or day" } },
            "/v1/usage/calls": { "get": { "summary": "Audit log" } },
            "/v1/usage/calls/{id}": { "get": { "summary": "One call with receipt and hash-chain proof" } },
            "/v1/balance": { "get": { "summary": "Mint balance, spend vs caps, top-up URL" } },
            "/v1/balance/topup": { "post": { "summary": "Start a Dodo top-up checkout through Mint" } },
            "/mcp": { "post": { "summary": "MCP streamable HTTP (JSON-RPC)" } },
        }
    });
    Json(doc).into_response()
}
