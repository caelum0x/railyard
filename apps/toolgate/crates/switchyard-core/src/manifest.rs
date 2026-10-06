//! Manifests: our own YAML format for capabilities, providers and tools.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::price::Price;

/// What a call does to the world. Drives scopes, MCP annotations and caching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffect {
    Read,
    Write,
    Send,
    Spend,
}

impl SideEffect {
    pub fn as_str(&self) -> &'static str {
        match self {
            SideEffect::Read => "read",
            SideEffect::Write => "write",
            SideEffect::Send => "send",
            SideEffect::Spend => "spend",
        }
    }
    /// The scope a caller needs to invoke a tool with this side effect.
    pub fn required_scope(&self) -> &'static str {
        match self {
            SideEffect::Read => "tools:call",
            SideEffect::Write => "tools:write",
            SideEffect::Send => "tools:send",
            SideEffect::Spend => "tools:spend",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "read" => Some(SideEffect::Read),
            "write" => Some(SideEffect::Write),
            "send" => Some(SideEffect::Send),
            "spend" => Some(SideEffect::Spend),
            _ => None,
        }
    }
}

/// Where the credential for a tool comes from (the credential ladder).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    /// Keyless public route.
    None,
    /// Needs a team-registered credential for the provider.
    ByoRequired,
    /// Works keyless; uses the team's credential when present (higher limits).
    ByoOptional,
    /// Our own product: the team's credential when stored, else the platform
    /// service credential from server config.
    OwnProduct,
    /// Uses the caller's own Relay key (one key, both rails).
    CallerKey,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    Own,
    Public,
    Byo,
    Platform,
}

impl ProviderKind {
    /// Routing tier under `free_first`: lower runs first.
    pub fn tier(&self) -> u8 {
        match self {
            ProviderKind::Own => 0,
            ProviderKind::Public => 1,
            ProviderKind::Byo => 2,
            ProviderKind::Platform => 3,
        }
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            ProviderKind::Own => "own",
            ProviderKind::Public => "public",
            ProviderKind::Byo => "byo",
            ProviderKind::Platform => "platform",
        }
    }
}

/// How a stored credential is placed on the upstream request.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CredentialPlacement {
    /// Header name, e.g. `Authorization` or `X-Subscription-Token`.
    #[serde(default)]
    pub header: Option<String>,
    /// Prefix put before the secret in the header, e.g. `Bearer `.
    #[serde(default)]
    pub prefix: Option<String>,
    /// Query parameter name, when the provider takes the key in the URL.
    #[serde(default)]
    pub query: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderManifest {
    pub id: String,
    pub title: String,
    pub kind: ProviderKind,
    /// Hosts egress may reach for this provider.
    #[serde(default)]
    pub egress_hosts: Vec<String>,
    /// Server config key whose URL host is added to the allowlist at startup
    /// (self-hosted SearXNG, our own product base URLs).
    #[serde(default)]
    pub egress_host_from_config: Option<String>,
    #[serde(default)]
    pub tos_resale_ok: bool,
    /// Etiquette: sustained requests per second and burst (token bucket).
    #[serde(default)]
    pub rate_per_s: Option<f64>,
    #[serde(default)]
    pub burst: Option<u32>,
    #[serde(default)]
    pub credential: Option<CredentialPlacement>,
    #[serde(default)]
    pub docs_url: Option<String>,
    /// OAuth 2 endpoints for connecting a team's own account
    /// (`POST /v1/credentials/oauth/{provider}/connect`).
    #[serde(default)]
    pub oauth: Option<OAuthSpec>,
}

/// A provider's OAuth 2 authorization-code endpoints (PKCE S256 is always used).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OAuthSpec {
    pub authorize_url: String,
    pub token_url: String,
    #[serde(default)]
    pub scopes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityManifest {
    pub id: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub input_schema: Value,
    pub output_schema: Value,
}

/// A declarative HTTP request template (rendered with minijinja; `input`,
/// `config` and `credential` are in scope).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestSpec {
    #[serde(default = "default_get")]
    pub method: String,
    pub url: String,
    /// Query parameters; a parameter whose template renders empty is omitted.
    #[serde(default)]
    pub query: BTreeMap<String, String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// JSON body; string leaves are templates.
    #[serde(default)]
    pub body: Option<Value>,
    /// Output mapping (see `switchyard-catalog::mapping`).
    #[serde(default)]
    pub output: Option<Value>,
    /// JSON Pointer into the mapped output whose array length is the result count.
    #[serde(default)]
    pub results_pointer: Option<String>,
}

fn default_get() -> String {
    "GET".into()
}
fn default_timeout() -> u64 {
    60
}
fn default_version() -> u32 {
    1
}
fn default_status() -> String {
    "active".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Example {
    pub input: Value,
    pub output: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolManifest {
    pub id: String,
    pub capability: String,
    pub provider: String,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub side_effect: SideEffect,
    pub price: Price,
    #[serde(default)]
    pub cache_ttl_s: u64,
    #[serde(default)]
    pub idempotent: bool,
    #[serde(default)]
    pub bill_on_4xx: bool,
    #[serde(default = "default_timeout")]
    pub timeout_s: u64,
    pub auth: AuthMode,
    /// Tool-specific schemas; when absent the capability's canonical schema applies.
    #[serde(default)]
    pub input_schema: Option<Value>,
    #[serde(default)]
    pub output_schema: Option<Value>,
    /// The tool accepts an input when it carries every field of at least one
    /// group. Empty means "anything that passes the schema".
    #[serde(default)]
    pub requires_any: Vec<Vec<String>>,
    /// Canonical input fields this provider cannot honour. An input that sets
    /// one is not accepted, so fallback never silently drops a constraint.
    #[serde(default)]
    pub unsupported_fields: Vec<String>,
    /// Declarative request; `None` means a native adapter serves it.
    #[serde(default)]
    pub request: Option<RequestSpec>,
    /// Native handler key, e.g. `rally.email.send`.
    #[serde(default)]
    pub native: Option<String>,
    #[serde(default)]
    pub example: Option<Example>,
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default = "default_status")]
    pub status: String,
}

impl ToolManifest {
    /// Does `input` satisfy one of the `requires_any` groups?
    pub fn accepts_fields(&self, input: &Value) -> Result<(), String> {
        if let Some(f) = self.unsupported_fields.iter().find(|f| input.get(f.as_str()).map(|v| !v.is_null()).unwrap_or(false)) {
            return Err(format!("{} cannot honour the `{f}` field", self.id));
        }
        if self.requires_any.is_empty() {
            return Ok(());
        }
        let has = |f: &String| input.get(f).map(|v| !v.is_null() && v != "").unwrap_or(false);
        if self.requires_any.iter().any(|group| group.iter().all(has)) {
            Ok(())
        } else {
            let groups: Vec<String> = self.requires_any.iter().map(|g| g.join("+")).collect();
            Err(format!("{} needs one of: {}", self.id, groups.join(" | ")))
        }
    }
}
