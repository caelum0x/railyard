//! Operator API for runtime-editable paid tools.
//!
//! All routes require `Authorization: Bearer <TURNSTILE_ADMIN_TOKEN>`.
//!
//! - `GET    /v1/admin/tools[?include_deleted=1]` — every tool with its full
//!   spec, lifecycle, version and live status, plus the credential env vars
//!   and upstream policy the editor needs;
//! - `POST   /v1/admin/tools` — create (body: a tool spec); `409` if the name
//!   is taken; re-creating a deleted name revives it;
//! - `GET    /v1/admin/tools/:name` — one tool;
//! - `PUT    /v1/admin/tools/:name` — replace the spec (optional `If-Match:
//!   <version>` for optimistic concurrency, `409` on a stale version);
//! - `POST   /v1/admin/tools/:name/enable` | `/disable`;
//! - `DELETE /v1/admin/tools/:name` — soft delete (route removed; settlement
//!   history and revenue for its resource are kept);
//! - `GET    /v1/admin/tool-audit[?tool=&limit=]` — who changed what, when.
//!
//! Every write is validated exactly like the seed file (spec rules, unique
//! name, credential present in the environment) plus the admin-API guardrails
//! in [`crate::policy`], then persisted with an audit entry and applied to the
//! live registry before the response returns. Other replicas converge on their
//! next reload poll.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use turnstile_core::{StoredTool, ToolStoreError};

use crate::live::{LiveRegistry, Snapshot, ECHO_TOOL};
use crate::operator::{error, OperatorState};
use crate::policy::UpstreamPolicy;
use crate::tools::{ToolSpec, UpstreamTool};

/// Largest accepted tool spec body.
const MAX_SPEC_BYTES: usize = 64 * 1024;
/// Most audit rows one call returns.
const MAX_AUDIT: usize = 500;

#[derive(Clone)]
pub struct AdminToolsState {
    pub operator: OperatorState,
    pub registry: Arc<LiveRegistry>,
    pub policy: Arc<UpstreamPolicy>,
}

fn store_error(err: ToolStoreError) -> Response {
    match err {
        ToolStoreError::NotFound(name) => error(
            StatusCode::NOT_FOUND,
            "unknown_tool",
            &format!("no paid tool named {name:?}"),
        ),
        ToolStoreError::Conflict(name) => error(
            StatusCode::CONFLICT,
            "tool_exists",
            &format!("a tool named {name:?} already exists"),
        ),
        ToolStoreError::VersionConflict { name, current } => (
            StatusCode::CONFLICT,
            Json(json!({
                "error": "version_conflict",
                "message": format!("{name} was changed by someone else (now version {current}); reload and retry"),
                "current_version": current,
            })),
        )
            .into_response(),
        ToolStoreError::Storage(message) => {
            eprintln!("tool store error: {message}");
            error(StatusCode::SERVICE_UNAVAILABLE, "store_unavailable", "tool store unavailable")
        }
    }
}

fn invalid(message: impl AsRef<str>) -> Response {
    error(StatusCode::UNPROCESSABLE_ENTITY, "invalid_tool", message.as_ref())
}

/// Operator identity for the audit trail: the console-reported operator name
/// plus the forwarding client address when behind a proxy.
fn actor(headers: &HeaderMap) -> String {
    let clean = |raw: &str| -> String {
        raw.chars()
            .filter(|c| c.is_ascii_graphic() || *c == ' ')
            .take(64)
            .collect::<String>()
            .trim()
            .to_string()
    };
    let name = headers
        .get("x-turnstile-operator")
        .and_then(|v| v.to_str().ok())
        .map(clean)
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "operator".to_string());
    match headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(clean)
        .filter(|ip| !ip.is_empty())
    {
        Some(ip) => format!("{name} ({ip})"),
        None => name,
    }
}

fn status_of(tool: &StoredTool, snapshot: &Snapshot) -> (&'static str, Option<String>) {
    if tool.deleted {
        ("deleted", None)
    } else if !tool.enabled {
        ("disabled", None)
    } else if let Some(reason) = snapshot.errors.get(&tool.name) {
        ("error", Some(reason.clone()))
    } else if snapshot.tools.contains_key(&tool.name) {
        ("live", None)
    } else {
        // Written but not yet applied on this replica.
        ("pending", None)
    }
}

fn tool_json(tool: &StoredTool, snapshot: &Snapshot) -> Value {
    let (status, reason) = status_of(tool, snapshot);
    json!({
        "name": tool.name,
        "spec": tool.spec,
        "enabled": tool.enabled,
        "deleted": tool.deleted,
        "version": tool.version,
        "created_at_ms": tool.created_at_ms,
        "updated_at_ms": tool.updated_at_ms,
        "builtin": false,
        "status": status,
        "error": reason,
    })
}

fn echo_row(registry: &LiveRegistry, snapshot: &Snapshot) -> Option<Value> {
    if !registry.echo_enabled() {
        return None;
    }
    let price = snapshot
        .catalog
        .tools
        .iter()
        .find(|t| t.name == ECHO_TOOL)
        .map(|t| t.price_micros.get())
        .unwrap_or_default();
    Some(json!({
        "name": ECHO_TOOL,
        "spec": {
            "name": ECHO_TOOL,
            "description": "Built-in demo: echoes the body back, billing one microdollar per byte.",
            "price_micros": price,
            "http_method": "POST",
            "http": true,
            "mcp": true,
        },
        "enabled": true,
        "deleted": false,
        "version": 0,
        "builtin": true,
        "status": "live",
        "error": null,
    }))
}

/// Reload after a local write so the response reflects the live state.
async fn apply(state: &AdminToolsState) -> (Arc<Snapshot>, Option<String>) {
    match state.registry.reload().await {
        Ok(snapshot) => (snapshot, None),
        Err(e) => {
            eprintln!("tool reload after admin write failed: {e}");
            (
                state.registry.snapshot(),
                Some("saved, but this replica could not reload yet; it will retry".into()),
            )
        }
    }
}

async fn written(state: &AdminToolsState, tool: StoredTool, action: &str, actor: &str) -> Response {
    println!(
        "admin audit: {actor} {action} paid tool {} (version {})",
        tool.name, tool.version
    );
    let (snapshot, warning) = apply(state).await;
    let mut body = tool_json(&tool, &snapshot);
    if let Some(warning) = warning {
        body["warning"] = json!(warning);
    }
    Json(body).into_response()
}

/// Validate a submitted spec against the file rules, the credential
/// environment, the reserved built-in name and the admin-API guardrails.
/// `existing` is the live stored version (for unchanged-value exemptions).
async fn vet(
    state: &AdminToolsState,
    body: &Bytes,
    path_name: Option<&str>,
    existing: Option<&StoredTool>,
) -> Result<ToolSpec, Response> {
    if body.len() > MAX_SPEC_BYTES {
        return Err(error(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "tool spec is too large"));
    }
    let spec: ToolSpec = serde_json::from_slice(body).map_err(|e| invalid(e.to_string()))?;
    if let Some(name) = path_name {
        if spec.name != name {
            return Err(invalid(format!(
                "spec name {:?} does not match the tool being edited ({name:?}); tools cannot be renamed",
                spec.name
            )));
        }
    }
    spec.validate().map_err(invalid)?;
    if state.registry.echo_enabled() && spec.name == ECHO_TOOL {
        return Err(error(
            StatusCode::CONFLICT,
            "reserved_name",
            "`echo` is reserved by the built-in demo tool (TURNSTILE_DEMO_ECHO=1)",
        ));
    }
    let previous: Option<ToolSpec> = existing
        .filter(|tool| !tool.deleted)
        .and_then(|tool| serde_json::from_value(tool.spec.clone()).ok());
    let url_changed = previous
        .as_ref()
        .map_or(true, |prev| prev.upstream_url != spec.upstream_url);
    if url_changed {
        state
            .policy
            .check_url(&spec.upstream_url)
            .await
            .map_err(invalid)?;
    }
    if let Some(var) = &spec.upstream_auth_env {
        let env_changed = previous
            .as_ref()
            .map_or(true, |prev| prev.upstream_auth_env.as_deref() != Some(var.as_str()));
        if env_changed {
            state.policy.check_auth_env(var).map_err(invalid)?;
        }
    }
    // Same credential check the router applies: the env var must be set here.
    UpstreamTool::new(spec.clone()).map_err(|e| {
        invalid(format!(
            "{e}. Set it in this server's environment (all replicas) and restart, or choose another credential"
        ))
    })?;
    Ok(spec)
}

fn spec_value(spec: &ToolSpec) -> Result<Value, Response> {
    serde_json::to_value(spec).map_err(|e| invalid(e.to_string()))
}

fn if_match(headers: &HeaderMap) -> Result<Option<i64>, Response> {
    let Some(raw) = headers.get("if-match").and_then(|v| v.to_str().ok()) else {
        return Ok(None);
    };
    let trimmed = raw.trim().trim_start_matches("W/").trim_matches('"');
    trimmed
        .parse::<i64>()
        .map(Some)
        .map_err(|_| invalid("If-Match must be the tool version number"))
}

#[derive(Deserialize, Default)]
pub struct ListQuery {
    include_deleted: Option<String>,
}

/// `GET /v1/admin/tools`
pub async fn list_tools(
    State(state): State<AdminToolsState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Response {
    if let Err(response) = state.operator.require_operator(&headers) {
        return response;
    }
    let include_deleted = matches!(query.include_deleted.as_deref(), Some("1" | "true" | "yes"));
    let rows = match state.registry.store().list().await {
        Ok(rows) => rows,
        Err(e) => return store_error(e),
    };
    let snapshot = state.registry.snapshot();
    let referenced: Vec<String> = rows
        .iter()
        .filter_map(|row| row.spec.get("upstream_auth_env").and_then(Value::as_str))
        .map(str::to_owned)
        .collect();
    let mut tools: Vec<Value> = echo_row(&state.registry, &snapshot).into_iter().collect();
    tools.extend(
        rows.iter()
            .filter(|row| include_deleted || !row.deleted)
            .map(|row| tool_json(row, &snapshot)),
    );
    Json(json!({
        "tools": tools,
        "revision": snapshot.revision,
        "payee": snapshot.catalog.payee,
        "currency": snapshot.catalog.currency,
        "auth_envs": state.policy.available_auth_envs(referenced),
        "policy": {
            "allow_hosts": state.policy.allow_hosts,
            "allow_private": state.policy.allow_private,
            "auth_env_prefix": crate::policy::UPSTREAM_ENV_PREFIX,
            "auth_env_allowlist": state.policy.auth_env_allowlist,
        },
    }))
    .into_response()
}

/// `GET /v1/admin/tools/:name`
pub async fn get_tool(
    State(state): State<AdminToolsState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Err(response) = state.operator.require_operator(&headers) {
        return response;
    }
    match state.registry.store().get(&name).await {
        Ok(Some(tool)) => Json(tool_json(&tool, &state.registry.snapshot())).into_response(),
        Ok(None) => store_error(ToolStoreError::NotFound(name)),
        Err(e) => store_error(e),
    }
}

/// `POST /v1/admin/tools`
pub async fn create_tool(
    State(state): State<AdminToolsState>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = state.operator.require_operator(&headers) {
        return response;
    }
    let spec = match vet(&state, &body, None, None).await {
        Ok(spec) => spec,
        Err(response) => return response,
    };
    let value = match spec_value(&spec) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let who = actor(&headers);
    match state.registry.store().create(&spec.name, &value, &who).await {
        Ok(tool) => {
            let mut response = written(&state, tool, "created", &who).await;
            *response.status_mut() = StatusCode::CREATED;
            response
        }
        Err(e) => store_error(e),
    }
}

/// `PUT /v1/admin/tools/:name`
pub async fn update_tool(
    State(state): State<AdminToolsState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    body: Bytes,
) -> Response {
    if let Err(response) = state.operator.require_operator(&headers) {
        return response;
    }
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(response) => return response,
    };
    let existing = match state.registry.store().get(&name).await {
        Ok(Some(tool)) if !tool.deleted => tool,
        Ok(_) => return store_error(ToolStoreError::NotFound(name)),
        Err(e) => return store_error(e),
    };
    let spec = match vet(&state, &body, Some(&name), Some(&existing)).await {
        Ok(spec) => spec,
        Err(response) => return response,
    };
    let value = match spec_value(&spec) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let who = actor(&headers);
    match state
        .registry
        .store()
        .update(&name, &value, expected, &who)
        .await
    {
        Ok(tool) => written(&state, tool, "updated", &who).await,
        Err(e) => store_error(e),
    }
}

async fn toggle(state: AdminToolsState, headers: HeaderMap, name: String, enabled: bool) -> Response {
    if let Err(response) = state.operator.require_operator(&headers) {
        return response;
    }
    let who = actor(&headers);
    match state.registry.store().set_enabled(&name, enabled, &who).await {
        Ok(tool) => {
            let action = if enabled { "enabled" } else { "disabled" };
            written(&state, tool, action, &who).await
        }
        Err(e) => store_error(e),
    }
}

/// `POST /v1/admin/tools/:name/enable`
pub async fn enable_tool(
    State(state): State<AdminToolsState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    toggle(state, headers, name, true).await
}

/// `POST /v1/admin/tools/:name/disable`
pub async fn disable_tool(
    State(state): State<AdminToolsState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    toggle(state, headers, name, false).await
}

/// `DELETE /v1/admin/tools/:name` — soft delete.
pub async fn delete_tool(
    State(state): State<AdminToolsState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> Response {
    if let Err(response) = state.operator.require_operator(&headers) {
        return response;
    }
    let who = actor(&headers);
    match state.registry.store().delete(&name, &who).await {
        Ok(tool) => written(&state, tool, "deleted", &who).await,
        Err(e) => store_error(e),
    }
}

#[derive(Deserialize, Default)]
pub struct AuditQuery {
    tool: Option<String>,
    limit: Option<usize>,
}

/// `GET /v1/admin/tool-audit`
pub async fn tool_audit(
    State(state): State<AdminToolsState>,
    headers: HeaderMap,
    Query(query): Query<AuditQuery>,
) -> Response {
    if let Err(response) = state.operator.require_operator(&headers) {
        return response;
    }
    let limit = query.limit.unwrap_or(100).clamp(1, MAX_AUDIT);
    let tool = query.tool.as_deref().filter(|t| !t.is_empty());
    match state.registry.store().audit(tool, limit).await {
        Ok(entries) => Json(json!({ "entries": entries })).into_response(),
        Err(e) => store_error(e),
    }
}
