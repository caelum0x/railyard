//! Config-driven paid tools: put ANY upstream HTTP endpoint behind a Turnstile
//! paywall without writing Rust.
//!
//! Tools live in the runtime-editable tool store (see [`crate::live`] and the
//! `/v1/admin/tools` API). `TURNSTILE_TOOLS_PATH` optionally points at a JSON
//! array of [`ToolSpec`]s used as a one-time seed while the store is empty.
//! For each enabled tool the server:
//! - builds its OWN gate priced at `price_micros` (so every 402 quotes exactly
//!   that tool's price) sharing the nonce store, Mint verifier and settlement
//!   journal;
//! - mounts `<http_method> /v1/tools/<name>/call` (default `POST`) behind that
//!   gate, proxying the paid request body and query string to `upstream_url`
//!   (success-only: a non-2xx upstream releases the hold, a 2xx captures);
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
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::extract::{FromRequestParts, Request};
use serde::{Deserialize, Serialize};
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Upstream HTTP method (`POST` default; `GET`, `PUT`, `PATCH`, `DELETE`).
    /// `GET`/`DELETE` upstreams receive no body: HTTP calls forward the query
    /// string, MCP calls send their flat scalar arguments as query parameters.
    #[serde(default = "default_method")]
    pub upstream_method: String,
    /// Method agents use on `/v1/tools/<name>/call` (`POST` default; `GET`,
    /// `PUT`, `PATCH`, `DELETE`). The 402 challenge binds to this method.
    #[serde(default = "default_method")]
    pub http_method: String,
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
        format!("{} {}", self.http_method, self.http_path())
    }

    /// Whether the upstream call carries no request body.
    pub fn upstream_is_bodyless(&self) -> bool {
        matches!(self.upstream_method.as_str(), "GET" | "DELETE")
    }

    /// The gate resource for the MCP tool.
    pub fn mcp_resource(&self) -> String {
        format!("tools/call#{}", self.name)
    }

    pub fn validate(&self) -> Result<(), String> {
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
        const METHODS: [&str; 5] = ["GET", "POST", "PUT", "PATCH", "DELETE"];
        if !METHODS.contains(&self.upstream_method.as_str()) {
            return Err(format!(
                "tool {} upstream_method must be GET, POST, PUT, PATCH or DELETE",
                self.name
            ));
        }
        if !METHODS.contains(&self.http_method.as_str()) {
            return Err(format!(
                "tool {} http_method must be GET, POST, PUT, PATCH or DELETE",
                self.name
            ));
        }
        if !self.http && !self.mcp {
            return Err(format!("tool {} must be exposed over http and/or mcp", self.name));
        }
        if self.timeout_ms == 0 || self.timeout_ms > 300_000 {
            return Err(format!("tool {} timeout_ms must be 1..=300000", self.name));
        }
        if self.description.trim().is_empty() || self.description.len() > 500 {
            return Err(format!("tool {} description must be 1..=500 characters", self.name));
        }
        if self.upstream_url.len() > 2048 {
            return Err(format!("tool {} upstream_url is too long", self.name));
        }
        if let Some(var) = &self.upstream_auth_env {
            let ok = !var.is_empty()
                && var.len() <= 128
                && var.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
            if !ok {
                return Err(format!("tool {} upstream_auth_env must be an env var name [A-Za-z0-9_]", self.name));
            }
        }
        if axum::http::HeaderName::from_bytes(self.upstream_auth_header.as_bytes()).is_err() {
            return Err(format!("tool {} upstream_auth_header is not a valid header name", self.name));
        }
        if let Some(header) = &self.usage_header {
            if axum::http::HeaderName::from_bytes(header.as_bytes()).is_err() {
                return Err(format!("tool {} usage_header is not a valid header name", self.name));
            }
        }
        if let Some(schema) = &self.input_schema {
            if !schema.is_object() {
                return Err(format!("tool {} input_schema must be a JSON object", self.name));
            }
        }
        Ok(())
    }
}

/// Load and validate the tool seed file. An empty array is allowed (the store
/// then starts empty and tools are added from the console).
pub fn load_specs(path: &str) -> Result<Vec<ToolSpec>, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("reading {path}: {e}"))?;
    let specs: Vec<ToolSpec> =
        serde_json::from_str(&raw).map_err(|e| format!("parsing {path}: {e}"))?;
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
    /// Re-checked on every [`call`](Self::call), not just at admin-write time —
    /// `upstream_url`'s host can legitimately resolve somewhere else later (DNS
    /// TTL expiry, the operator's registrar changing hands, an attacker who
    /// controls the domain re-pointing it) long after `check_url` approved it
    /// once. A snapshot's `UpstreamTool` is also reused unchanged across every
    /// reload while the tool's own row is untouched (see `live::activate`), so
    /// without a per-call check the write-time approval would otherwise never
    /// be revisited for the tool's entire lifetime.
    policy: Arc<crate::policy::UpstreamPolicy>,
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
    /// Bind a spec, resolving its credential from the environment. `policy` is
    /// re-applied to `upstream_url` on every [`call`](Self::call), not just here.
    pub fn new(spec: ToolSpec, policy: Arc<crate::policy::UpstreamPolicy>) -> Result<Self, String> {
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
        Ok(Self { spec, client, auth, policy })
    }

    /// Forward a paid call upstream. `query` (already URL-encoded, without the
    /// leading `?`) is appended to `upstream_url`.
    pub async fn call(
        &self,
        content_type: Option<&str>,
        body: Bytes,
        query: Option<&str>,
    ) -> Result<UpstreamReply, String> {
        // Re-validate the upstream host NOW, not just whenever an operator last
        // created/edited this tool's URL (see the `policy` field doc): a host
        // that resolved publicly then can resolve to a private/loopback/
        // metadata address now. `allow_private`/`allow_hosts` are honored
        // identically to the write-time check.
        self.policy
            .check_url(&self.spec.upstream_url)
            .await
            .map_err(|e| format!("upstream blocked by policy: {e}"))?;
        let method = reqwest::Method::from_bytes(self.spec.upstream_method.as_bytes())
            .map_err(|e| e.to_string())?;
        let url = match query.filter(|q| !q.is_empty()) {
            Some(q) if self.spec.upstream_url.contains('?') => format!("{}&{q}", self.spec.upstream_url),
            Some(q) => format!("{}?{q}", self.spec.upstream_url),
            None => self.spec.upstream_url.clone(),
        };
        let mut request = self.client.request(method, &url);
        if !self.spec.upstream_is_bodyless() {
            request = request
                .header("content-type", content_type.unwrap_or("application/json"))
                .body(body.to_vec());
        }
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

/// Encode MCP `arguments` as a query string for a bodyless (`GET`/`DELETE`)
/// upstream. Only a flat object of scalars can be expressed this way.
pub fn arguments_to_query(args: &Value) -> Result<String, String> {
    let Some(map) = args.as_object() else {
        return Ok(String::new());
    };
    let mut pairs = Vec::with_capacity(map.len());
    for (key, value) in map {
        let text = match value {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Null => continue,
            _ => {
                return Err(format!(
                    "argument {key:?} must be a string, number or boolean for a {} upstream",
                    "GET/DELETE"
                ))
            }
        };
        pairs.push(format!("{}={}", percent_encode(key), percent_encode(&text)));
    }
    Ok(pairs.join("&"))
}

/// RFC 3986 percent-encoding of everything except unreserved characters.
fn percent_encode(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `<http_method> /v1/tools/<name>/call` — the paid proxy body, run by
/// [`turnstile_axum::gate_request`] only after the payment was admitted.
pub async fn proxy_request(tool: Arc<UpstreamTool>, req: Request) -> Response {
    let (mut parts, body) = req.into_parts();
    let paid = match PaidRequest::from_request_parts(&mut parts, &()).await {
        Ok(paid) => paid,
        Err(response) => return response,
    };
    let body = match axum::body::to_bytes(body, MAX_BODY_BYTES).await {
        Ok(body) => body,
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, "request body too large").into_response(),
    };
    let content_type = parts.headers.get("content-type").and_then(|v| v.to_str().ok());
    match tool.call(content_type, body, parts.uri.query()).await {
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

#[cfg(test)]
mod tests {
    //! Regression test for the bug found 2026-10-29: `UpstreamTool::call` had
    //! no SSRF guard of its own — `UpstreamPolicy::check_url` only ran once,
    //! at admin-write time, on the URL string. A hostname that resolved
    //! publicly then (passing the check) can resolve to a loopback/private/
    //! metadata address at any later moment (DNS TTL expiry, a re-pointed
    //! domain) and every subsequent paid call would proxy straight to it with
    //! zero re-validation, for the tool's entire lifetime — exactly the SSRF
    //! this module's own doc comment says a leaked admin token must not grant.
    use super::*;
    use crate::policy::UpstreamPolicy;

    fn loopback_spec(url: String) -> ToolSpec {
        ToolSpec {
            name: "poc".into(),
            description: "poc".into(),
            price_micros: 1,
            upstream_url: url,
            upstream_method: "POST".into(),
            http_method: "POST".into(),
            upstream_auth_env: None,
            upstream_auth_header: default_auth_header(),
            usage_header: None,
            input_schema: None,
            http: true,
            mcp: false,
            timeout_ms: default_timeout_ms(),
        }
    }

    #[tokio::test]
    async fn call_refuses_a_loopback_upstream_even_though_new_already_accepted_it() {
        // A real local "internal" server an attacker would want to reach. If
        // `UpstreamTool::new` had been given a loopback URL directly it would
        // already be caught by admin-write-time `check_url` — the live bug
        // was that nothing re-checked it once the tool existed, which this
        // models by handing `new` the (production, unmodified) policy but
        // never routing this exact URL through `check_url` until `call` does
        // it internally.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let body = b"INTERNAL-SECRET-DATA";
            let resp = format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", body.len());
            let _ = socket.write_all(resp.as_bytes()).await;
            let _ = socket.write_all(body).await;
        });
        let loopback_url = format!("http://127.0.0.1:{}/", addr.port());
        let policy = Arc::new(UpstreamPolicy::default());
        let tool = UpstreamTool::new(loopback_spec(loopback_url), policy).unwrap();

        let result = tool
            .call(Some("application/json"), Bytes::from_static(b"{}"), None)
            .await;
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("call() must refuse a loopback upstream, not proxy to it"),
        };
        assert!(err.contains("upstream blocked by policy"), "{err}");
    }

    #[tokio::test]
    async fn call_still_reaches_an_allowed_upstream() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 1024];
            let _ = socket.read(&mut buf).await;
            let body = b"ok";
            let resp = format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", body.len());
            let _ = socket.write_all(resp.as_bytes()).await;
            let _ = socket.write_all(body).await;
        });
        let loopback_url = format!("http://127.0.0.1:{}/", addr.port());
        let policy = Arc::new(UpstreamPolicy {
            allow_private: true,
            ..UpstreamPolicy::default()
        });
        let tool = UpstreamTool::new(loopback_spec(loopback_url), policy).unwrap();
        let reply = tool
            .call(Some("application/json"), Bytes::from_static(b"{}"), None)
            .await
            .unwrap();
        assert_eq!(reply.status, StatusCode::OK);
        assert_eq!(&reply.body[..], b"ok");
    }
}
