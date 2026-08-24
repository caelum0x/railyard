//! The injectable HTTP transport seam for the Mint-backed ledger.
//!
//! [`MintLedger`](crate::mint::MintLedger) never touches `reqwest` directly — it
//! speaks to this small [`HttpTransport`] trait. Production wiring injects
//! [`ReqwestTransport`] (real HTTP against mint-server); tests inject a fake that
//! records the exact requests and replays canned mint-server responses. This is
//! the boundary that keeps the ledger's protocol logic unit-testable without a
//! live server or a network.

use std::time::Duration;

use async_trait::async_trait;

/// Total request budget for a single mint-server call. Bounds reserve/settle/
/// release so a hung mint-server surfaces as a [`TransportError`] instead of
/// blocking the billing middleware forever (which would leave holds un-released).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Connection-establishment budget, a tighter bound within [`REQUEST_TIMEOUT`].
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// A transport-level failure (DNS, connection, TLS, timeout, or a malformed
/// request the transport could not send). Protocol/`4xx` errors are NOT modeled
/// here — those come back as a non-2xx [`HttpResponse`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("mint transport error: {0}")]
pub struct TransportError(pub String);

/// An outbound HTTP request against mint-server.
///
/// Header names are lowercased (HTTP headers are case-insensitive); the body is
/// already-serialized JSON bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    /// `"POST"` or `"GET"`.
    pub method: &'static str,
    /// Path only (e.g. `"/v1/holds"`); the transport prepends the base URL.
    pub path: String,
    /// Lowercased header name/value pairs (e.g. `("x-api-key", "…")`).
    pub headers: Vec<(String, String)>,
    /// Serialized JSON request body (empty for GET).
    pub body: Vec<u8>,
}

impl HttpRequest {
    /// A `GET` with no body.
    pub fn get(path: impl Into<String>) -> Self {
        Self {
            method: "GET",
            path: path.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }

    /// A `POST` carrying `body` as `application/json`.
    pub fn post_json(path: impl Into<String>, body: Vec<u8>) -> Self {
        Self {
            method: "POST",
            path: path.into(),
            headers: vec![("content-type".to_string(), "application/json".to_string())],
            body,
        }
    }

    /// Append a header (name is lowercased for case-insensitive matching).
    #[must_use]
    pub fn with_header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers
            .push((name.into().to_ascii_lowercase(), value.into()));
        self
    }

    /// The request body decoded as a UTF-8 string (helper for test assertions).
    #[must_use]
    pub fn body_str(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// An HTTP response from mint-server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// HTTP status code (e.g. `200`, `201`, `402`).
    pub status: u16,
    /// Lowercased header name/value pairs.
    pub headers: Vec<(String, String)>,
    /// Raw response body bytes.
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// Construct a response with a body and no headers.
    pub fn new(status: u16, body: impl Into<Vec<u8>>) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: body.into(),
        }
    }

    /// `true` for `2xx`.
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The injectable transport. Implementors carry the base URL; the per-request
/// `X-API-Key` header is attached by the ledger, so the transport stays a dumb
/// pipe.
#[async_trait]
pub trait HttpTransport: Send + Sync {
    /// Send `req` and return the response, or a [`TransportError`] on a
    /// transport-level failure.
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, TransportError>;
}

/// The production [`HttpTransport`]: a `reqwest` client bound to a base URL.
///
/// Construct via [`ReqwestTransport::new`], passing the mint-server base URL
/// (e.g. `https://mint.internal`). The trailing slash is trimmed so paths join
/// cleanly.
pub struct ReqwestTransport {
    client: reqwest::Client,
    base_url: String,
}

impl ReqwestTransport {
    /// A transport for `base_url` (trailing slash trimmed), with bounded
    /// request and connect timeouts so a slow or hung mint-server fails fast
    /// (as a [`TransportError`]) rather than blocking indefinitely. Fails only
    /// if the underlying TLS/client build fails.
    pub fn new(base_url: impl Into<String>) -> Result<Self, TransportError> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .map_err(|e| TransportError(format!("http client build failed: {e}")))?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
        })
    }

    /// Reuse an already-configured client (e.g. one with timeouts/proxy set).
    pub fn with_client(client: reqwest::Client, base_url: impl Into<String>) -> Self {
        Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }
}

#[async_trait]
impl HttpTransport for ReqwestTransport {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, TransportError> {
        let url = format!("{}{}", self.base_url, req.path);
        let method = reqwest::Method::from_bytes(req.method.as_bytes())
            .map_err(|e| TransportError(format!("bad method {}: {e}", req.method)))?;

        let mut builder = self.client.request(method, &url);
        for (name, value) in &req.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if !req.body.is_empty() {
            builder = builder.body(req.body);
        }

        let resp = builder
            .send()
            .await
            .map_err(|e| TransportError(format!("request to {url} failed: {e}")))?;

        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(k, v)| {
                (
                    k.as_str().to_ascii_lowercase(),
                    v.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect();
        let body = resp
            .bytes()
            .await
            .map_err(|e| TransportError(format!("reading body from {url} failed: {e}")))?
            .to_vec();

        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}
