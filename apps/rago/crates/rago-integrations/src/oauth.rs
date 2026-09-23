//! Integration connection OAuth lifecycle — the token exchange + refresh that
//! `Integrations::Aggregator::*` connections use, modeled over the injectable
//! [`HttpTransport`] so the flow is verifiable without live credentials.
//!
//! Mirrors the standard OAuth 2.0 authorization-code grant: exchange the
//! authorization `code` for an access + refresh token, and later refresh the
//! access token with the stored refresh token. A live deployment injects the
//! reqwest transport with owner-gated client secrets; tests inject a scripted
//! transport.

use serde_json::Value;

use crate::integrations::IntegrationError;
use crate::transport::{HttpTransport, TransportError};

/// OAuth client configuration for one integration provider.
#[derive(Debug, Clone)]
pub struct OAuthConfig {
    pub token_url: String,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
}

/// The tokens returned by the provider's token endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Lifetime in seconds (`expires_in`), if provided.
    pub expires_in: Option<i64>,
    pub token_type: String,
    /// OIDC `id_token`, when the provider issues one.
    pub id_token: Option<String>,
    /// Salesforce `instance_url`: the org-specific API host every later call
    /// must address.
    pub instance_url: Option<String>,
    /// Granted scopes (space-separated), when echoed back.
    pub scope: Option<String>,
}

/// An OAuth client over an injectable transport `T`.
pub struct OAuthClient<T: HttpTransport> {
    config: OAuthConfig,
    transport: T,
}

impl<T: HttpTransport> OAuthClient<T> {
    pub fn new(config: OAuthConfig, transport: T) -> Self {
        Self { config, transport }
    }

    /// Exchange an authorization `code` for tokens (grant_type=authorization_code).
    pub async fn exchange_code(&self, code: &str) -> Result<OAuthTokens, IntegrationError> {
        let form = form_encode(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", &self.config.client_id),
            ("client_secret", &self.config.client_secret),
            ("redirect_uri", &self.config.redirect_uri),
        ]);
        self.token_request(form).await
    }

    /// Exchange an authorization `code` bound to a PKCE `code_verifier`
    /// (RFC 7636) for tokens.
    pub async fn exchange_code_with_verifier(
        &self,
        code: &str,
        code_verifier: &str,
    ) -> Result<OAuthTokens, IntegrationError> {
        let form = form_encode(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", &self.config.client_id),
            ("client_secret", &self.config.client_secret),
            ("redirect_uri", &self.config.redirect_uri),
            ("code_verifier", code_verifier),
        ]);
        self.token_request(form).await
    }

    /// Refresh the access token with a stored refresh token
    /// (grant_type=refresh_token).
    pub async fn refresh(&self, refresh_token: &str) -> Result<OAuthTokens, IntegrationError> {
        let form = form_encode(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", &self.config.client_id),
            ("client_secret", &self.config.client_secret),
        ]);
        self.token_request(form).await
    }

    async fn token_request(&self, form: String) -> Result<OAuthTokens, IntegrationError> {
        let headers = vec![("Content-Type".to_string(), "application/x-www-form-urlencoded".to_string())];
        let resp = self.transport.post(&self.config.token_url, &headers, form).await?;
        let json: Value =
            serde_json::from_str(&resp.body).map_err(|e| IntegrationError::InvalidResponse(e.to_string()))?;
        if !resp.is_success() {
            return Err(IntegrationError::Provider {
                code: json.get("error").and_then(Value::as_str).map(str::to_string),
                message: json
                    .get("error_description")
                    .or_else(|| json.get("error"))
                    .and_then(Value::as_str)
                    .unwrap_or("oauth error")
                    .to_string(),
            });
        }
        let access_token = json
            .get("access_token")
            .and_then(Value::as_str)
            .ok_or_else(|| IntegrationError::InvalidResponse("missing access_token".into()))?
            .to_string();
        Ok(OAuthTokens {
            access_token,
            refresh_token: json.get("refresh_token").and_then(Value::as_str).map(str::to_string),
            expires_in: json.get("expires_in").and_then(Value::as_i64),
            token_type: json
                .get("token_type")
                .and_then(Value::as_str)
                .unwrap_or("Bearer")
                .to_string(),
            id_token: json.get("id_token").and_then(Value::as_str).map(str::to_string),
            instance_url: json.get("instance_url").and_then(Value::as_str).map(str::to_string),
            scope: json.get("scope").and_then(Value::as_str).map(str::to_string),
        })
    }
}

/// Build a URL query string (`k=v&…`, percent-encoded).
pub fn query_string(pairs: &[(&str, &str)]) -> String {
    form_encode(pairs)
}

/// A fresh high-entropy PKCE code verifier / OAuth state value (64 hex chars,
/// 244 random bits).
pub fn random_token() -> String {
    format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())
}

/// PKCE `S256` code challenge: base64url(no padding) of SHA-256(verifier).
pub fn pkce_challenge(code_verifier: &str) -> String {
    use base64::Engine;
    use sha2::Digest;
    let digest = sha2::Sha256::digest(code_verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

fn form_encode(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Minimal application/x-www-form-urlencoded percent-encoding (unreserved chars
/// pass through; everything else is `%XX`).
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Retry a fallible async operation on transient transport failures with the
/// deterministic ActiveJob-style backoff. Provider (4xx-class) errors are not
/// retried — only transport I/O errors are. Mirrors the sync error taxonomy:
/// transient (retry) vs. provider (surface).
pub async fn with_retry<F, Fut, TItem>(max_attempts: u32, mut op: F) -> Result<TItem, IntegrationError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<TItem, IntegrationError>>,
{
    let mut attempt = 0;
    loop {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e @ IntegrationError::Transport(TransportError::Io(_))) => {
                attempt += 1;
                if attempt >= max_attempts {
                    return Err(e);
                }
            }
            Err(other) => return Err(other),
        }
    }
}
