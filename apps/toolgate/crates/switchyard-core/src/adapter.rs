//! The provider adapter interface and the context a call runs in.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;

use crate::http::{EgressError, HttpTransport};
use crate::manifest::ToolManifest;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialKind {
    ApiKey,
    Oauth,
    Basic,
    HeaderSet,
}

impl CredentialKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            CredentialKind::ApiKey => "api_key",
            CredentialKind::Oauth => "oauth",
            CredentialKind::Basic => "basic",
            CredentialKind::HeaderSet => "header_set",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "api_key" => Some(CredentialKind::ApiKey),
            "oauth" => Some(CredentialKind::Oauth),
            "basic" => Some(CredentialKind::Basic),
            "header_set" => Some(CredentialKind::HeaderSet),
            _ => None,
        }
    }
}

/// A credential decrypted just in time. The secret is zeroised on drop
/// (`SecretString`) and never serialised or logged.
#[derive(Clone)]
pub struct Credential {
    pub id: String,
    pub provider: String,
    pub kind: CredentialKind,
    pub secret: SecretString,
    /// Non-secret metadata (base URL for BYO, OAuth expiry, project ids).
    pub meta: Value,
    /// `true` when this is the platform's own service credential rather than
    /// the team's (drives metering: team keys are unmetered).
    pub platform: bool,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("id", &self.id)
            .field("provider", &self.provider)
            .field("kind", &self.kind)
            .field("secret", &"[redacted]")
            .field("platform", &self.platform)
            .finish()
    }
}

impl Credential {
    pub fn expose(&self) -> &str {
        self.secret.expose_secret()
    }
}

/// Everything an adapter may use while serving one call.
#[derive(Clone)]
pub struct CallCtx {
    pub call_id: String,
    pub project_id: String,
    pub org_id: String,
    pub http: Arc<dyn HttpTransport>,
    pub credential: Option<Credential>,
    /// The caller's own Relay key (for `auth: caller_key` tools).
    pub caller_key: Option<SecretString>,
    /// Server configuration values templates and natives may read
    /// (`searxng_url`, `rally_url`, `contact_email`, ...). Never secrets.
    pub config: Arc<BTreeMap<String, String>>,
    pub timeout: Duration,
    /// The caller's Idempotency-Key (forwarded to providers that support one,
    /// e.g. Rally send, so a retry never sends twice).
    pub idempotency_key: Option<String>,
}

impl CallCtx {
    pub fn config(&self, key: &str) -> Option<&str> {
        self.config.get(key).map(|s| s.as_str()).filter(|s| !s.is_empty())
    }
    /// The User-Agent every upstream request carries (public API etiquette).
    pub fn user_agent(&self) -> String {
        let contact = self.config("contact_email").unwrap_or("ops@railyard.dev");
        format!("Switchyard/0.1 (+https://railyard.dev/switchyard; {contact})")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Acceptance {
    Yes,
    No(String),
}

/// A successful adapter result.
#[derive(Debug, Clone, PartialEq)]
pub struct AdapterOutput {
    pub data: Value,
    /// Upstream status of the final attempt (for settlement and stats).
    pub status: u16,
    /// Units consumed (results returned, recipients, seconds, 1k chars).
    pub units: Option<u64>,
    /// Provider-reported charge in micros, when the provider reports one.
    pub reported_cost_micros: Option<i64>,
}

impl AdapterOutput {
    pub fn ok(data: Value) -> Self {
        AdapterOutput { data, status: 200, units: None, reported_cost_micros: None }
    }
    pub fn with_units(mut self, units: u64) -> Self {
        self.units = Some(units);
        self
    }
}

/// Error classes drive fallback and billing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// 5xx, timeouts, network, rate limits: try the next candidate. Never billed.
    Retryable,
    /// Misconfiguration or policy refusal: stop. Never billed.
    Fatal,
    /// The provider rejected the input (4xx): stop, not billed.
    InputRejected,
    /// A 4xx the provider charges for (tool marked `bill_on_4xx`).
    ProviderBilled4xx,
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{class:?}: {message}")]
pub struct AdapterError {
    pub class: ErrorClass,
    pub message: String,
    pub status: Option<u16>,
}

impl AdapterError {
    pub fn retryable(message: impl Into<String>) -> Self {
        AdapterError { class: ErrorClass::Retryable, message: message.into(), status: None }
    }
    pub fn fatal(message: impl Into<String>) -> Self {
        AdapterError { class: ErrorClass::Fatal, message: message.into(), status: None }
    }
    pub fn rejected(message: impl Into<String>) -> Self {
        AdapterError { class: ErrorClass::InputRejected, message: message.into(), status: None }
    }
    pub fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    /// Classify an upstream HTTP status. 429 and 5xx are retryable; 3xx left
    /// over after egress followed redirects is fatal; other 4xx are input
    /// rejections unless the tool bills 4xx.
    pub fn from_status(status: u16, body: &str, bill_on_4xx: bool) -> Self {
        let snippet: String = body.chars().take(300).collect();
        let message = format!("upstream returned {status}: {snippet}");
        let class = match status {
            429 | 500..=599 => ErrorClass::Retryable,
            400..=499 if bill_on_4xx => ErrorClass::ProviderBilled4xx,
            400..=499 => ErrorClass::InputRejected,
            _ => ErrorClass::Fatal,
        };
        AdapterError { class, message, status: Some(status) }
    }
}

impl From<EgressError> for AdapterError {
    fn from(e: EgressError) -> Self {
        if e.is_policy() {
            AdapterError::fatal(e.to_string())
        } else {
            AdapterError::retryable(e.to_string())
        }
    }
}

#[async_trait]
pub trait Adapter: Send + Sync {
    /// Stable provider id, e.g. `open-meteo`, `rally`.
    fn provider(&self) -> &str;
    /// Tools this adapter serves.
    fn tools(&self) -> &[ToolManifest];
    /// Can this adapter satisfy this input? Default: the manifest's `requires_any`.
    fn accepts(&self, tool: &str, input: &Value) -> Acceptance {
        match self.tools().iter().find(|t| t.id == tool) {
            None => Acceptance::No(format!("{tool} is not served by {}", self.provider())),
            Some(m) => match m.accepts_fields(input) {
                Ok(()) => Acceptance::Yes,
                Err(why) => Acceptance::No(why),
            },
        }
    }
    /// Upper-bound price quote in micros (the hold size). Default: the manifest price.
    fn quote(&self, tool: &str, input: &Value) -> i64 {
        self.tools().iter().find(|t| t.id == tool).map(|m| m.price.quote(input)).unwrap_or(0)
    }
    /// Execute the call.
    async fn call(&self, ctx: &CallCtx, tool: &str, input: Value) -> Result<AdapterOutput, AdapterError>;
    /// Optional cheap liveness probe.
    async fn probe(&self, _ctx: &CallCtx) -> Result<(), AdapterError> {
        Ok(())
    }
}
