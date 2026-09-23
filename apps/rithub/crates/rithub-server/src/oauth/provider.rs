//! Provider abstraction for the OAuth2 authorization-code login flow.
//!
//! Every external identity provider is reached through the [`OauthProvider`]
//! trait: build the authorize URL, exchange an authorization code for an access
//! token, and fetch normalized user info. Two implementations exist:
//!
//! - [`HttpProvider`] — talks to a real IdP over HTTPS (`reqwest`). GitHub and
//!   Google are supported out of the box with sensible default endpoints; every
//!   endpoint is overridable via environment so a self-hoster can point at an
//!   enterprise / GHES instance.
//! - [`MockProvider`] — enabled with `RITHUB_OAUTH_MOCK=1`. It loops the browser
//!   straight back to our own callback with a canned code and returns a
//!   deterministic identity, so the whole flow is exercisable end-to-end with no
//!   real client credentials.
//!
//! ## Configuration (environment)
//! For each provider `P` in `{GITHUB, GOOGLE}`:
//! - `RITHUB_OAUTH_P_CLIENT_ID`     — OAuth client id      (required)
//! - `RITHUB_OAUTH_P_CLIENT_SECRET` — OAuth client secret  (required)
//! - `RITHUB_OAUTH_P_AUTH_URL`      — authorize endpoint    (has a default)
//! - `RITHUB_OAUTH_P_TOKEN_URL`     — token endpoint        (has a default)
//! - `RITHUB_OAUTH_P_USERINFO_URL`  — userinfo endpoint     (has a default)
//! - `RITHUB_OAUTH_P_SCOPE`         — space-delimited scope (has a default)
//!
//! Secrets are read from the environment only and are **never** logged: error
//! variants carry provider slugs and HTTP statuses, never credentials or tokens.

use std::time::Duration;

use serde::Deserialize;

/// Normalized identity distilled from a provider's userinfo endpoint.
#[derive(Debug, Clone)]
pub struct OauthUserInfo {
    /// The provider's stable, opaque subject id for this account.
    pub provider_user_id: String,
    /// A human login/handle to seed a forge username from (may be empty).
    pub username: String,
    /// The account email (may be empty if the provider withholds it).
    pub email: String,
}

/// Failures in the provider exchange. Messages are for server logs only; the
/// web handlers surface a generic page and never echo a secret or token.
#[derive(Debug, thiserror::Error)]
pub enum OauthError {
    #[error("unknown oauth provider: {0}")]
    UnknownProvider(String),
    #[error("oauth provider not configured: {0}")]
    NotConfigured(String),
    #[error("token exchange failed: {0}")]
    Exchange(String),
    #[error("userinfo fetch failed: {0}")]
    UserInfo(String),
}

/// The set of external identity providers this build understands.
pub const SUPPORTED_PROVIDERS: &[&str] = &["github", "google"];

/// A single provider's resolved configuration (endpoints + client credentials).
#[derive(Clone, Debug)]
pub struct ProviderConfig {
    pub name: String,
    pub client_id: String,
    pub client_secret: String,
    pub auth_url: String,
    pub token_url: String,
    pub userinfo_url: String,
    pub scope: String,
}

impl ProviderConfig {
    /// Read a provider's config from the environment, applying per-provider
    /// endpoint/scope defaults. Returns `None` for an unsupported provider slug.
    /// `client_id` / `client_secret` may be empty here — emptiness is treated as
    /// "not configured" by [`provider_for`].
    pub fn from_env(name: &str) -> Option<Self> {
        let (auth, token, userinfo, scope) = defaults(name)?;
        let key = name.to_ascii_uppercase();
        let var = |suffix: &str| std::env::var(format!("RITHUB_OAUTH_{key}_{suffix}")).ok();
        Some(Self {
            name: name.to_string(),
            client_id: var("CLIENT_ID").unwrap_or_default(),
            client_secret: var("CLIENT_SECRET").unwrap_or_default(),
            auth_url: var("AUTH_URL").unwrap_or_else(|| auth.to_string()),
            token_url: var("TOKEN_URL").unwrap_or_else(|| token.to_string()),
            userinfo_url: var("USERINFO_URL").unwrap_or_else(|| userinfo.to_string()),
            scope: var("SCOPE").unwrap_or_else(|| scope.to_string()),
        })
    }
}

/// Default (authorize, token, userinfo, scope) endpoints per provider.
fn defaults(name: &str) -> Option<(&'static str, &'static str, &'static str, &'static str)> {
    match name {
        "github" => Some((
            "https://github.com/login/oauth/authorize",
            "https://github.com/login/oauth/access_token",
            "https://api.github.com/user",
            "read:user user:email",
        )),
        "google" => Some((
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://oauth2.googleapis.com/token",
            "https://openidconnect.googleapis.com/v1/userinfo",
            "openid email profile",
        )),
        _ => None,
    }
}

/// Whether the mock provider is enabled (`RITHUB_OAUTH_MOCK=1` / `true`).
pub fn mock_enabled() -> bool {
    std::env::var("RITHUB_OAUTH_MOCK")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

/// Whether a provider is available for login: mock on, or real credentials set.
pub fn is_enabled(name: &str) -> bool {
    if !SUPPORTED_PROVIDERS.contains(&name) {
        return false;
    }
    if mock_enabled() {
        return true;
    }
    ProviderConfig::from_env(name)
        .map(|c| !c.client_id.is_empty() && !c.client_secret.is_empty())
        .unwrap_or(false)
}

/// Resolve a provider slug to a concrete [`OauthProvider`] implementation.
pub fn provider_for(name: &str) -> Result<Box<dyn OauthProvider>, OauthError> {
    if !SUPPORTED_PROVIDERS.contains(&name) {
        return Err(OauthError::UnknownProvider(name.to_string()));
    }
    if mock_enabled() {
        return Ok(Box::new(MockProvider::new(name)));
    }
    let cfg = ProviderConfig::from_env(name)
        .ok_or_else(|| OauthError::UnknownProvider(name.to_string()))?;
    if cfg.client_id.is_empty() || cfg.client_secret.is_empty() {
        return Err(OauthError::NotConfigured(name.to_string()));
    }
    Ok(Box::new(HttpProvider::new(cfg)))
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// A pluggable OAuth2 identity provider. Trait-object safe via `async_trait`.
#[async_trait::async_trait]
pub trait OauthProvider: Send + Sync {
    /// Build the provider's authorization-endpoint URL to redirect the browser
    /// to. Carries the anti-CSRF `state` and the PKCE `code_challenge` (S256).
    fn authorize_url(&self, redirect_uri: &str, state: &str, pkce_challenge: &str) -> String;

    /// Exchange a freshly-issued authorization `code` for an access token,
    /// binding the exchange to the same `redirect_uri` and PKCE `verifier`.
    async fn exchange_code(
        &self,
        redirect_uri: &str,
        code: &str,
        pkce_verifier: &str,
    ) -> Result<String, OauthError>;

    /// Fetch and normalize the signed-in user's profile with the access token.
    async fn fetch_userinfo(&self, access_token: &str) -> Result<OauthUserInfo, OauthError>;
}

// ---------------------------------------------------------------------------
// Real HTTP provider
// ---------------------------------------------------------------------------

/// A provider backed by real HTTPS calls to an external IdP.
pub struct HttpProvider {
    cfg: ProviderConfig,
}

impl HttpProvider {
    pub fn new(cfg: ProviderConfig) -> Self {
        Self { cfg }
    }
}

/// Build a short-timeout HTTP client. Falls back to a default client if the
/// builder fails (e.g. TLS backend init) so a request still gets attempted.
fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap_or_default()
}

#[async_trait::async_trait]
impl OauthProvider for HttpProvider {
    fn authorize_url(&self, redirect_uri: &str, state: &str, pkce_challenge: &str) -> String {
        reqwest::Url::parse_with_params(
            &self.cfg.auth_url,
            &[
                ("client_id", self.cfg.client_id.as_str()),
                ("redirect_uri", redirect_uri),
                ("response_type", "code"),
                ("scope", self.cfg.scope.as_str()),
                ("state", state),
                ("code_challenge", pkce_challenge),
                ("code_challenge_method", "S256"),
            ],
        )
        .map(|u| u.to_string())
        // A malformed configured auth_url should not panic the request; the
        // browser will simply land on an invalid URL and the login won't
        // complete. This is a misconfiguration, logged at the call site.
        .unwrap_or_default()
    }

    async fn exchange_code(
        &self,
        redirect_uri: &str,
        code: &str,
        pkce_verifier: &str,
    ) -> Result<String, OauthError> {
        // Client secret and code travel in the POST body (never the URL/logs).
        let params = [
            ("grant_type", "authorization_code"),
            ("client_id", self.cfg.client_id.as_str()),
            ("client_secret", self.cfg.client_secret.as_str()),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", pkce_verifier),
        ];
        let resp = http_client()
            .post(&self.cfg.token_url)
            .header(reqwest::header::ACCEPT, "application/json")
            .header(reqwest::header::USER_AGENT, "rithub")
            .form(&params)
            .send()
            .await
            .map_err(|e| OauthError::Exchange(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(OauthError::Exchange(format!("provider returned {status}")));
        }

        #[derive(Deserialize)]
        struct TokenResponse {
            access_token: Option<String>,
            error: Option<String>,
            error_description: Option<String>,
        }
        let body: TokenResponse = resp
            .json()
            .await
            .map_err(|e| OauthError::Exchange(e.to_string()))?;
        if let Some(err) = body.error {
            let detail = body.error_description.unwrap_or(err);
            return Err(OauthError::Exchange(detail));
        }
        body.access_token
            .filter(|t| !t.is_empty())
            .ok_or_else(|| OauthError::Exchange("no access_token in response".into()))
    }

    async fn fetch_userinfo(&self, access_token: &str) -> Result<OauthUserInfo, OauthError> {
        let resp = http_client()
            .get(&self.cfg.userinfo_url)
            .header(reqwest::header::ACCEPT, "application/json")
            // GitHub's API rejects requests without a User-Agent.
            .header(reqwest::header::USER_AGENT, "rithub")
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(|e| OauthError::UserInfo(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            return Err(OauthError::UserInfo(format!("provider returned {status}")));
        }
        let value: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| OauthError::UserInfo(e.to_string()))?;

        let info = normalize_userinfo(&self.cfg.name, &value);
        if info.provider_user_id.is_empty() {
            return Err(OauthError::UserInfo("provider omitted a subject id".into()));
        }
        Ok(info)
    }
}

/// Map a provider's raw userinfo JSON onto the normalized shape.
fn normalize_userinfo(provider: &str, v: &serde_json::Value) -> OauthUserInfo {
    let str_field = |key: &str| v.get(key).and_then(|x| x.as_str()).map(str::to_string);

    match provider {
        "github" => {
            // GitHub `/user`: numeric `id`, string `login`, nullable `email`.
            let provider_user_id = v
                .get("id")
                .map(|x| match x {
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                })
                .unwrap_or_default();
            let username = str_field("login").unwrap_or_default();
            let email = str_field("email")
                .filter(|s| !s.is_empty())
                // A user who hides their email still gets a stable noreply
                // address, matching what GitHub itself hands out.
                .unwrap_or_else(|| {
                    if username.is_empty() {
                        String::new()
                    } else {
                        format!("{username}@users.noreply.github.com")
                    }
                });
            OauthUserInfo {
                provider_user_id,
                username,
                email,
            }
        }
        // Google + any OpenID-Connect-shaped userinfo: `sub`, `email`, `name`.
        _ => {
            let provider_user_id = str_field("sub").unwrap_or_default();
            let email = str_field("email").unwrap_or_default();
            let username = str_field("preferred_username")
                .or_else(|| str_field("name"))
                .unwrap_or_else(|| email.split('@').next().unwrap_or_default().to_string());
            OauthUserInfo {
                provider_user_id,
                username,
                email,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Mock provider (RITHUB_OAUTH_MOCK=1)
// ---------------------------------------------------------------------------

/// A credential-free provider that makes the whole flow exercisable in dev/CI.
///
/// [`authorize_url`](OauthProvider::authorize_url) loops the browser directly
/// back to our own callback with a canned `code`, so no external IdP is
/// contacted. The returned identity is deterministic and can be overridden with
/// `RITHUB_OAUTH_MOCK_{SUBJECT,USERNAME,EMAIL}` to exercise find-or-create and
/// account-linking paths.
pub struct MockProvider {
    name: String,
}

impl MockProvider {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
        }
    }
}

#[async_trait::async_trait]
impl OauthProvider for MockProvider {
    fn authorize_url(&self, redirect_uri: &str, state: &str, _pkce_challenge: &str) -> String {
        reqwest::Url::parse_with_params(redirect_uri, &[("code", "mock-code"), ("state", state)])
            .map(|u| u.to_string())
            .unwrap_or_else(|_| redirect_uri.to_string())
    }

    async fn exchange_code(
        &self,
        _redirect_uri: &str,
        code: &str,
        _pkce_verifier: &str,
    ) -> Result<String, OauthError> {
        if code.is_empty() {
            return Err(OauthError::Exchange("empty authorization code".into()));
        }
        Ok(format!("mock-access-token-{}", self.name))
    }

    async fn fetch_userinfo(&self, _access_token: &str) -> Result<OauthUserInfo, OauthError> {
        let or_default = |suffix: &str, fallback: String| {
            std::env::var(format!("RITHUB_OAUTH_MOCK_{suffix}")).unwrap_or(fallback)
        };
        let provider_user_id = or_default("SUBJECT", format!("mock-{}-1", self.name));
        let username = or_default("USERNAME", format!("mock-{}", self.name));
        let email = or_default("EMAIL", format!("{username}@oauth.local"));
        Ok(OauthUserInfo {
            provider_user_id,
            username,
            email,
        })
    }
}
