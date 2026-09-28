//! The SSRF-safe [`HttpTransport`].

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use switchyard_core::{EgressError, HttpRequest, HttpResponse, HttpTransport};
use url::{Host, Url};

use crate::policy::host_allowed;
use crate::resolver::{PinningResolver, ReqwestPins, Resolver, SystemResolver};

pub const MAX_REDIRECTS: usize = 3;
const STRIPPED: &[&str] = &[
    "authorization", "cookie", "proxy-authorization", "host", "connection", "keep-alive", "te", "trailer",
    "transfer-encoding", "upgrade", "proxy-connection", "content-length",
];

#[derive(Clone)]
pub struct EgressConfig {
    pub allow_hosts: BTreeSet<String>,
    pub connect_timeout: Duration,
    /// Allow plain http (tests only; production is https-only).
    pub allow_http: bool,
    /// Addresses exempt from the block list (tests only).
    pub allow_ips: Vec<IpAddr>,
    pub user_agent: String,
}

impl Default for EgressConfig {
    fn default() -> Self {
        EgressConfig {
            allow_hosts: BTreeSet::new(),
            connect_timeout: Duration::from_secs(10),
            allow_http: false,
            allow_ips: Vec::new(),
            user_agent: "Switchyard/0.1".into(),
        }
    }
}

pub struct SafeClient {
    http: reqwest::Client,
    pins: Arc<PinningResolver>,
    allow_hosts: RwLock<BTreeSet<String>>,
    allow_http: bool,
    user_agent: String,
}

impl SafeClient {
    pub fn new(config: EgressConfig) -> Result<Self, EgressError> {
        Self::with_resolver(config, Arc::new(SystemResolver))
    }

    pub fn with_resolver(config: EgressConfig, resolver: Arc<dyn Resolver>) -> Result<Self, EgressError> {
        let pins = Arc::new(PinningResolver::new(resolver, config.allow_ips.clone()));
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(config.connect_timeout)
            .no_proxy()
            .https_only(!config.allow_http)
            .dns_resolver(Arc::new(ReqwestPins(pins.clone())))
            .build()
            .map_err(|e| EgressError::Network(e.to_string()))?;
        Ok(SafeClient {
            http,
            pins,
            allow_hosts: RwLock::new(config.allow_hosts),
            allow_http: config.allow_http,
            user_agent: config.user_agent,
        })
    }

    /// Add hosts at runtime (admin-registered BYO hosts).
    pub fn allow_host(&self, host: &str) {
        self.allow_hosts.write().expect("allowlist poisoned").insert(host.to_ascii_lowercase());
    }

    pub fn allowed_hosts(&self) -> BTreeSet<String> {
        self.allow_hosts.read().expect("allowlist poisoned").clone()
    }

    /// Validate one hop: scheme, host allowlist, and every resolved address.
    async fn check_hop(&self, url: &Url, extra: &[String]) -> Result<(), EgressError> {
        match url.scheme() {
            "https" => {}
            "http" if self.allow_http => {}
            other => return Err(EgressError::Scheme(format!("{other}://"))),
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(EgressError::InvalidUrl("credentials in URL are not allowed".into()));
        }
        let host = url.host().ok_or_else(|| EgressError::InvalidUrl(url.to_string()))?;
        match host {
            // IP literals (including decimal/hex/octal forms, which the URL
            // parser normalises to dotted IPv4) never go through DNS, so they are
            // checked here and must be explicitly allowlisted.
            Host::Ipv4(ip) => self.check_literal(IpAddr::V4(ip), extra),
            Host::Ipv6(ip) => self.check_literal(IpAddr::V6(ip), extra),
            Host::Domain(name) => {
                let allowed = {
                    let allow = self.allow_hosts.read().expect("allowlist poisoned");
                    host_allowed(name, allow.iter()) || host_allowed(name, extra.iter())
                };
                if !allowed {
                    return Err(EgressError::HostNotAllowed(name.to_string()));
                }
                self.pins.resolve_and_pin(name).await.map(|_| ())
            }
        }
    }

    fn check_literal(&self, ip: IpAddr, extra: &[String]) -> Result<(), EgressError> {
        if !self.pins.ip_allowed(ip) {
            return Err(EgressError::BlockedAddress(ip.to_string()));
        }
        let text = ip.to_string();
        let allow = self.allow_hosts.read().expect("allowlist poisoned");
        if host_allowed(&text, allow.iter()) || host_allowed(&text, extra.iter()) {
            Ok(())
        } else {
            Err(EgressError::HostNotAllowed(text))
        }
    }

    async fn send_inner(&self, req: HttpRequest) -> Result<HttpResponse, EgressError> {
        let mut url = Url::parse(&req.url).map_err(|e| EgressError::InvalidUrl(format!("{}: {e}", req.url)))?;
        let origin_host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        let mut method = req.method.clone();
        let mut body = req.body.clone();
        for hop in 0..=MAX_REDIRECTS {
            self.check_hop(&url, &req.extra_hosts).await?;
            let same_host = url.host_str().map(|h| h.eq_ignore_ascii_case(&origin_host)).unwrap_or(false);
            let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| EgressError::InvalidUrl(e.to_string()))?;
            let mut rb = self.http.request(m, url.clone()).header("user-agent", &self.user_agent);
            for (k, v) in &req.headers {
                if STRIPPED.contains(&k.to_ascii_lowercase().as_str()) {
                    continue;
                }
                rb = rb.header(k.as_str(), v.as_str());
            }
            if same_host {
                for (k, v) in &req.sensitive_headers {
                    rb = rb.header(k.as_str(), v.as_str());
                }
            }
            if let Some(b) = &body {
                rb = rb.body(b.clone());
            }
            let mut resp = rb.send().await.map_err(|e| {
                if e.is_timeout() {
                    EgressError::Timeout
                } else {
                    EgressError::Network(e.to_string())
                }
            })?;
            let status = resp.status().as_u16();
            if (300..400).contains(&status) && status != 304 {
                if let Some(loc) = resp.headers().get("location").and_then(|v| v.to_str().ok()) {
                    if hop == MAX_REDIRECTS {
                        return Err(EgressError::TooManyRedirects);
                    }
                    url = url.join(loc).map_err(|e| EgressError::InvalidUrl(format!("redirect {loc}: {e}")))?;
                    if status == 303 || ((status == 301 || status == 302) && method == "POST") {
                        method = "GET".into();
                        body = None;
                    }
                    continue;
                }
            }
            let headers = resp
                .headers()
                .iter()
                .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), v.to_string())))
                .collect();
            if let Some(len) = resp.content_length() {
                if len as usize > req.max_body_bytes {
                    return Err(EgressError::TooLarge(req.max_body_bytes));
                }
            }
            let mut buf = Vec::new();
            while let Some(chunk) = resp.chunk().await.map_err(|e| {
                if e.is_timeout() {
                    EgressError::Timeout
                } else {
                    EgressError::Network(e.to_string())
                }
            })? {
                if buf.len() + chunk.len() > req.max_body_bytes {
                    return Err(EgressError::TooLarge(req.max_body_bytes));
                }
                buf.extend_from_slice(&chunk);
            }
            return Ok(HttpResponse { status, headers, body: buf, final_url: url.to_string() });
        }
        Err(EgressError::TooManyRedirects)
    }
}

#[async_trait]
impl HttpTransport for SafeClient {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, EgressError> {
        let timeout = req.timeout;
        match tokio::time::timeout(timeout, self.send_inner(req)).await {
            Ok(r) => r,
            Err(_) => Err(EgressError::Timeout),
        }
    }
}
