//! The outbound HTTP seam. Every upstream request any adapter makes goes
//! through an [`HttpTransport`]; production wires the SSRF-safe egress client.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

/// One outbound request.
#[derive(Debug, Clone)]
pub struct HttpRequest {
    pub method: String,
    pub url: String,
    /// Ordinary headers (content type, accept, user agent).
    pub headers: Vec<(String, String)>,
    /// Credential headers. Egress sends them to the original host only and drops
    /// them if a redirect leaves that host. Never logged.
    pub sensitive_headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    pub timeout: Duration,
    pub max_body_bytes: usize,
    /// Extra hosts allowed for this request only (e.g. an RDAP server named by
    /// IANA's bootstrap file). Still subject to the IP block list.
    pub extra_hosts: Vec<String>,
}

impl HttpRequest {
    pub fn get(url: impl Into<String>) -> Self {
        HttpRequest {
            method: "GET".into(),
            url: url.into(),
            headers: Vec::new(),
            sensitive_headers: Vec::new(),
            body: None,
            timeout: Duration::from_secs(60),
            max_body_bytes: 16 * 1024 * 1024,
            extra_hosts: Vec::new(),
        }
    }

    pub fn post_json(url: impl Into<String>, body: &Value) -> Self {
        let mut req = Self::get(url);
        req.method = "POST".into();
        req.headers.push(("content-type".into(), "application/json".into()));
        req.body = Some(body.to_string().into_bytes());
        req
    }

    pub fn header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.push((name.to_string(), value.into()));
        self
    }

    pub fn secret_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.sensitive_headers.push((name.to_string(), value.into()));
        self
    }

    pub fn timeout(mut self, t: Duration) -> Self {
        self.timeout = t;
        self
    }
}

/// One upstream response (fully buffered, bounded by `max_body_bytes`).
#[derive(Debug, Clone)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
    pub final_url: String,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> Result<Value, String> {
        serde_json::from_slice(&self.body).map_err(|e| format!("upstream returned non-JSON body: {e}"))
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Why egress refused or failed a request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EgressError {
    #[error("invalid url: {0}")]
    InvalidUrl(String),
    #[error("only https is allowed: {0}")]
    Scheme(String),
    #[error("host is not on the egress allowlist: {0}")]
    HostNotAllowed(String),
    #[error("address is blocked (private, loopback, link-local, metadata or multicast): {0}")]
    BlockedAddress(String),
    #[error("too many redirects")]
    TooManyRedirects,
    #[error("response exceeds {0} bytes")]
    TooLarge(usize),
    #[error("upstream timed out")]
    Timeout,
    #[error("network error: {0}")]
    Network(String),
}

impl EgressError {
    /// Policy refusals are fatal (retrying elsewhere will not help this URL);
    /// timeouts and network errors are retryable on another provider.
    pub fn is_policy(&self) -> bool {
        matches!(
            self,
            EgressError::InvalidUrl(_) | EgressError::Scheme(_) | EgressError::HostNotAllowed(_) | EgressError::BlockedAddress(_)
        )
    }
}

#[async_trait]
pub trait HttpTransport: Send + Sync {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, EgressError>;
}
