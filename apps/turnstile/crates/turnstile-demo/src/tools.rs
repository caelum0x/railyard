//! Config-driven paid tools: put ANY upstream HTTP endpoint behind a Turnstile
//! paywall without writing Rust.
//!
//! `TURNSTILE_TOOLS_PATH` points at a JSON array of [`ToolSpec`]s. For each
//! tool the server:
//! - builds its OWN gate priced at `price_micros` (so every 402 quotes exactly
//!   that tool's price) sharing the nonce store, Mint verifier and settlement
//!   journal;
//! - mounts `POST /v1/tools/<name>/call` behind that gate, proxying the paid
//!   request body to `upstream_url` (success-only: a non-2xx upstream releases
//!   the hold, a 2xx captures);
//! - exposes the same tool over MCP (`tools/call` with `name = <name>`), sending
//!   the call `arguments` to the upstream as the JSON body;
//! - advertises it in `/v1/tools` + `/.well-known/aitp-tools`.
//!
//! Metered billing: if the upstream answers with the header named by
//! `usage_header` (integer microdollars), that amount — capped at the quoted
//! price — is captured instead of the full quote.
//!
//! Upstream credentials never live in the file: `upstream_auth_env` names an
//! environment variable whose value is sent in `upstream_auth_header`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::Value;
use turnstile_axum::PaidRequest;
use turnstile_core::AmountMicros;

/// Largest request/response body proxied (bytes).
const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

fn default_true() -> bool {
    true
}

fn default_method() -> String {
    "POST".into()
}

fn default_auth_header() -> String {
    "authorization".into()
}

fn default_timeout_ms() -> u64 {
    30_000
}

/// One operator-declared paid tool.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSpec {
    /// Stable tool name: `[a-z0-9_-]{1,64}`.
    pub name: String,
    /// One-line description shown in discovery and MCP `tools/list`.
    pub description: String,
    /// Price per call in integer microdollars.
    pub price_micros: i64,
    /// Upstream endpoint the paid call is forwarded to.
    pub upstream_url: String,
    /// Upstream HTTP method (`POST` default, or `PUT`/`PATCH`).
    #[serde(default = "default_method")]
    pub upstream_method: String,
    /// Env var holding the upstream credential (optional).
    #[serde(default)]
    pub upstream_auth_env: Option<String>,
    /// Header the credential is sent in (default `authorization`).
    #[serde(default = "default_auth_header")]
    pub upstream_auth_header: String,
    /// Response header carrying actual usage in µ$ (optional metered billing).
    #[serde(default)]
    pub usage_header: Option<String>,
    /// JSON Schema for MCP `tools/call` arguments (default: any object).
    #[serde(default)]
    pub input_schema: Option<Value>,
    /// Expose over HTTP (`POST /v1/tools/<name>/call`).
    #[serde(default = "default_true")]
    pub http: bool,
    /// Expose over MCP.
    #[serde(default = "default_true")]
    pub mcp: bool,
    /// Upstream timeout.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

impl ToolSpec {
    /// The HTTP route this tool is served on.
    pub fn http_path(&self) -> String {
        format!("/v1/tools/{}/call", self.name)
    }

    /// The gate resource for the HTTP route (`"<METHOD> <path>"`).
    pub fn http_resource(&self) -> String {
        format!("POST {}", self.http_path())
    }

    /// The gate resource for the MCP tool.
    pub fn mcp_resource(&self) -> String {
        format!("tools/call#{}", self.name)
    }

    fn validate(&self) -> Result<(), String> {
        let name_ok = !self.name.is_empty()
            && self.name.len() <= 64
            && self
                .name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
        if !name_ok {
            return Err(format!("tool name {:?} must match [a-z0-9_-]{{1,64}}", self.name));
        }
        if self.price_micros < 0 {
            return Err(format!("tool {} price_micros must be non-negative", self.name));
        }
        if !(self.upstream_url.starts_with("https://") || self.upstream_url.starts_with("http://")) {
            return Err(format!("tool {} upstream_url must be http(s)", self.name));
        }
        if !matches!(self.upstream_method.as_str(), "POST" | "PUT" | "PATCH") {
            return Err(format!("tool {} upstream_method must be POST, PUT or PATCH", self.name));
        }
        if !self.http && !self.mcp {
            return Err(format!("tool {} must be exposed over http and/or mcp", self.name));
        }
        if self.timeout_ms == 0 || self.timeout_ms > 300_000 {
            return Err(format!("tool {} timeout_ms must be 1..=300000", self.name));
        }
        Ok(())
    }
}

/// Load and validate the tool registry file.
pub fn load_specs(path: &str) -> Result<Vec<ToolSpec>, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("reading {path}: {e}"))?;
    let specs: Vec<ToolSpec> =
        serde_json::from_str(&raw).map_err(|e| format!("parsing {path}: {e}"))?;
    if specs.is_empty() {
        return Err(format!("{path} declares no tools"));
    }
    let mut seen = std::collections::HashSet::new();
    for spec in &specs {
        spec.validate()?;
        if !seen.insert(spec.name.clone()) {
            return Err(format!("duplicate tool name {}", spec.name));
        }
    }
    Ok(specs)
}

/// A live upstream binding for one tool.
pub struct UpstreamTool {
    pub spec: ToolSpec,
    client: reqwest::Client,
    auth: Option<(String, String)>,
}

/// Outcome of one upstream call.
pub struct UpstreamReply {
    pub status: StatusCode,
    pub content_type: Option<String>,
    pub body: Bytes,
    /// Usage reported by the upstream in µ$, if metered.
    pub usage_micros: Option<i64>,
}

impl UpstreamTool {
    /// Bind a spec, resolving its credential from the environment.
    pub fn new(spec: ToolSpec) -> Result<Self, String> {
        let auth = match &spec.upstream_auth_env {
            Some(var) => {
                let value = std::env::var(var)
                    .ok()
                    .filter(|v| !v.trim().is_empty())
                    .ok_or_else(|| format!("tool {}: env var {var} is not set", spec.name))?;
                Some((spec.upstream_auth_header.to_ascii_lowercase(), value))
            }
            None => None,
        };
        let client = reqwest::Client::builder()
            .timeout(Duration::from_millis(spec.timeout_ms))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| format!("tool {}: building HTTP client: {e}", spec.name))?;
        Ok(Self { spec, client, auth })
    }

    /// Forward a paid call upstream.
    pub async fn call(&self, content_type: Option<&str>, body: Bytes) -> Result<UpstreamReply, String> {
        let method = reqwest::Method::from_bytes(self.spec.upstream_method.as_bytes())
            .map_err(|e| e.to_string())?;
        let mut request = self
            .client
            .request(method, &self.spec.upstream_url)
            .header("content-type", content_type.unwrap_or("application/json"))
            .body(body.to_vec());
        if let Some((header, value)) = &self.auth {
            request = request.header(header.as_str(), value.as_str());
        }
        let response = request
            .send()
            .await
            .map_err(|e| format!("upstream unreachable: {e}"))?;
        let status = StatusCode::from_u16(response.status().as_u16())
            .unwrap_or(StatusCode::BAD_GATEWAY);
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let usage_micros = self.spec.usage_header.as_ref().and_then(|name| {
            response
                .headers()
                .get(name.as_str())
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<i64>().ok())
        });
        let body = response
            .bytes()
            .await
            .map_err(|e| format!("reading upstream body: {e}"))?;
        if body.len() > MAX_BODY_BYTES {
            return Err("upstream response exceeds the proxy size limit".into());
        }
        Ok(UpstreamReply {
            status,
            content_type,
            body,
            usage_micros,
        })
    }

    /// The amount to capture for a successful call: metered usage capped at the
    /// quote, or the full quote when the upstream reports nothing.
    pub fn billable(&self, quoted: i64, usage_micros: Option<i64>) -> AmountMicros {
        AmountMicros::new(usage_micros.map_or(quoted, |u| u.clamp(0, quoted)))
    }
}

/// `POST /v1/tools/<name>/call` — the paid proxy handler (behind `admit`).
pub async fn proxy_http(
    State(tool): State<Arc<UpstreamTool>>,
    paid: PaidRequest,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if body.len() > MAX_BODY_BYTES {
        return (StatusCode::PAYLOAD_TOO_LARGE, "request body too large").into_response();
    }
    let content_type = headers.get("content-type").and_then(|v| v.to_str().ok());
    match tool.call(content_type, body).await {
        Ok(reply) => {
            if reply.status.is_success() {
                paid.report_actual(tool.billable(paid.price().get(), reply.usage_micros));
            }
            let mut response = (reply.status, reply.body).into_response();
            if let Some(ct) = reply.content_type.and_then(|ct| HeaderValue::from_str(&ct).ok()) {
                response.headers_mut().insert("content-type", ct);
            }
            response
        }
        // Non-2xx → the gate releases the hold; nothing is charged.
        Err(error) => (StatusCode::BAD_GATEWAY, error).into_response(),
    }
}
