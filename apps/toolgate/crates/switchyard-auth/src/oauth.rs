//! OAuth 2.1 authorization server logic (transport-free; the server crate
//! mounts it on axum routes).

use std::sync::Arc;

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use switchyard_core::{new_id, HttpRequest, HttpTransport};
use switchyard_store::{DeviceCode, OAuthClient, OAuthCode, OAuthGrant, Store};

use crate::jwt::{AccessClaims, TokenSigner};
use crate::{hash_token, random_token, scopes};

pub const CLI_CLIENT_ID: &str = "switchyard-cli";
pub const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OAuthError {
    #[serde(skip)]
    pub status: u16,
    pub error: String,
    pub error_description: String,
}

impl OAuthError {
    pub fn new(status: u16, error: &str, description: impl Into<String>) -> Self {
        OAuthError { status, error: error.into(), error_description: description.into() }
    }
    fn invalid_request(d: impl Into<String>) -> Self {
        Self::new(400, "invalid_request", d)
    }
    fn invalid_grant(d: impl Into<String>) -> Self {
        Self::new(400, "invalid_grant", d)
    }
    fn invalid_client(d: impl Into<String>) -> Self {
        Self::new(401, "invalid_client", d)
    }
    fn server(d: impl std::fmt::Display) -> Self {
        Self::new(500, "server_error", d.to_string())
    }
}

#[derive(Debug, Clone)]
pub struct OAuthConfig {
    /// Public base URL, e.g. `https://rail.example.com`.
    pub issuer: String,
    /// Resources a token may be bound to (`{issuer}/mcp` and the API itself).
    pub resources: Vec<String>,
    pub access_ttl: Duration,
    pub refresh_ttl: Duration,
    pub code_ttl: Duration,
    pub device_ttl: Duration,
}

impl OAuthConfig {
    pub fn for_issuer(issuer: &str) -> Self {
        let issuer = issuer.trim_end_matches('/').to_string();
        OAuthConfig {
            resources: vec![format!("{issuer}/mcp"), issuer.clone()],
            issuer,
            access_ttl: Duration::minutes(10),
            refresh_ttl: Duration::days(30),
            code_ttl: Duration::minutes(5),
            device_ttl: Duration::minutes(10),
        }
    }
    pub fn mcp_resource(&self) -> String {
        format!("{}/mcp", self.issuer)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct AuthorizeParams {
    pub response_type: String,
    pub client_id: String,
    pub redirect_uri: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    pub code_challenge: Option<String>,
    pub code_challenge_method: Option<String>,
    #[serde(default)]
    pub resource: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct TokenRequest {
    pub grant_type: String,
    pub code: Option<String>,
    pub redirect_uri: Option<String>,
    pub code_verifier: Option<String>,
    pub client_id: Option<String>,
    pub refresh_token: Option<String>,
    pub device_code: Option<String>,
    pub resource: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub token_type: String,
    pub expires_in: i64,
    pub refresh_token: String,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceStart {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    pub expires_in: i64,
    pub interval: i64,
}

/// `BASE64URL(SHA256(verifier)) == challenge` (S256 only).
pub fn pkce_s256_matches(verifier: &str, challenge: &str) -> bool {
    if verifier.len() < 43 || verifier.len() > 128 {
        return false;
    }
    let computed = B64.encode(Sha256::digest(verifier.as_bytes()));
    subtle::ConstantTimeEq::ct_eq(computed.as_bytes(), challenge.as_bytes()).into()
}

fn is_loopback_http(u: &url::Url) -> bool {
    u.scheme() == "http" && matches!(u.host_str(), Some("127.0.0.1") | Some("localhost") | Some("[::1]"))
}

/// Redirect URIs: https, or http on loopback (native apps / CLIs).
pub fn valid_redirect_uri(uri: &str) -> bool {
    match url::Url::parse(uri) {
        Ok(u) => u.fragment().is_none() && (u.scheme() == "https" || is_loopback_http(&u)),
        Err(_) => false,
    }
}

fn redirect_matches(registered: &[String], given: &str) -> bool {
    registered.iter().any(|r| {
        if r == given {
            return true;
        }
        // Loopback redirects may use any port (RFC 8252 §7.3).
        match (url::Url::parse(r), url::Url::parse(given)) {
            (Ok(a), Ok(b)) if is_loopback_http(&a) && is_loopback_http(&b) => a.host_str() == b.host_str() && a.path() == b.path(),
            _ => false,
        }
    })
}

fn user_code() -> String {
    const ALPHABET: &[u8] = b"BCDFGHJKLMNPQRSTVWXZ";
    let mut out = String::new();
    for i in 0..8 {
        if i == 4 {
            out.push('-');
        }
        out.push(ALPHABET[(rand::random::<u32>() as usize) % ALPHABET.len()] as char);
    }
    out
}

pub struct AuthServer {
    pub store: Arc<dyn Store>,
    pub signer: Arc<TokenSigner>,
    pub cfg: OAuthConfig,
    /// Egress used to fetch client ID metadata documents.
    pub http: Option<Arc<dyn HttpTransport>>,
}

impl AuthServer {
    pub fn authorization_server_metadata(&self) -> Value {
        let i = &self.cfg.issuer;
        json!({
            "issuer": i,
            "authorization_endpoint": format!("{i}/oauth/authorize"),
            "token_endpoint": format!("{i}/oauth/token"),
            "registration_endpoint": format!("{i}/oauth/register"),
            "device_authorization_endpoint": format!("{i}/oauth/device/code"),
            "jwks_uri": format!("{i}/oauth/jwks"),
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token", DEVICE_GRANT],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none"],
            "scopes_supported": scopes::ALL,
            "client_id_metadata_document_supported": true,
        })
    }

    pub fn protected_resource_metadata(&self, resource: &str) -> Value {
        json!({
            "resource": resource,
            "authorization_servers": [self.cfg.issuer],
            "scopes_supported": scopes::ALL,
            "bearer_methods_supported": ["header"],
            "resource_documentation": format!("{}/llms.txt", self.cfg.issuer),
        })
    }

    /// Dynamic client registration (RFC 7591), public clients only.
    pub async fn register(&self, body: &Value) -> Result<OAuthClient, OAuthError> {
        let uris: Vec<String> = body
            .get("redirect_uris")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        if uris.is_empty() || uris.len() > 10 || !uris.iter().all(|u| valid_redirect_uri(u)) {
            return Err(OAuthError::new(400, "invalid_redirect_uri", "redirect_uris must be https or http loopback"));
        }
        if let Some(m) = body.get("token_endpoint_auth_method").and_then(Value::as_str) {
            if m != "none" {
                return Err(OAuthError::new(400, "invalid_client_metadata", "only public clients (token_endpoint_auth_method=none)"));
            }
        }
        let name = body.get("client_name").and_then(Value::as_str).unwrap_or("MCP client").chars().take(100).collect::<String>();
        let client = OAuthClient {
            client_id: new_id("dcr"),
            kind: "dcr".into(),
            metadata: json!({ "client_name": name, "token_endpoint_auth_method": "none", "grant_types": ["authorization_code", "refresh_token"] }),
            redirect_uris: uris,
            created_at: Utc::now(),
        };
        self.store.insert_client(client.clone()).await.map_err(OAuthError::server)
    }

    /// Look a client up; an `https://` client id is a client ID metadata
    /// document, fetched through egress and cached for 24 h.
    pub async fn resolve_client(&self, client_id: &str) -> Result<OAuthClient, OAuthError> {
        if client_id == CLI_CLIENT_ID {
            return Ok(OAuthClient { client_id: CLI_CLIENT_ID.into(), kind: "first_party".into(), metadata: json!({"client_name": "Switchyard CLI"}), redirect_uris: vec![], created_at: Utc::now() });
        }
        let cached = self.store.get_client(client_id).await.map_err(OAuthError::server)?;
        let is_cimd = client_id.starts_with("https://");
        match cached {
            Some(c) if !is_cimd || c.created_at > Utc::now() - Duration::hours(24) => return Ok(c),
            None if !is_cimd => return Err(OAuthError::invalid_client("unknown client_id")),
            _ => {}
        }
        let http = self.http.as_ref().ok_or_else(|| OAuthError::invalid_client("client metadata documents are disabled"))?;
        let url = url::Url::parse(client_id).map_err(|_| OAuthError::invalid_client("client_id is not a URL"))?;
        if url.path() == "/" || url.fragment().is_some() {
            return Err(OAuthError::invalid_client("client_id metadata URL must have a path"));
        }
        let mut req = HttpRequest::get(client_id).header("accept", "application/json");
        req.extra_hosts = vec![url.host_str().unwrap_or_default().to_string()];
        req.max_body_bytes = 64 * 1024;
        req.timeout = std::time::Duration::from_secs(5);
        let resp = http.send(req).await.map_err(|e| OAuthError::invalid_client(format!("client metadata fetch failed: {e}")))?;
        if resp.status != 200 {
            return Err(OAuthError::invalid_client(format!("client metadata returned {}", resp.status)));
        }
        let doc = resp.json().map_err(OAuthError::invalid_client)?;
        if doc.get("client_id").and_then(Value::as_str) != Some(client_id) {
            return Err(OAuthError::invalid_client("metadata client_id must equal the document URL"));
        }
        let uris: Vec<String> = doc
            .get("redirect_uris")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        if uris.is_empty() || !uris.iter().all(|u| valid_redirect_uri(u)) {
            return Err(OAuthError::invalid_client("metadata redirect_uris must be https or http loopback"));
        }
        let client = OAuthClient { client_id: client_id.into(), kind: "cimd".into(), metadata: doc, redirect_uris: uris, created_at: Utc::now() };
        self.store.insert_client(client.clone()).await.map_err(OAuthError::server)
    }

    /// Validate an authorization request before showing consent. Returns the
    /// client and the requested (known) scopes.
    pub async fn validate_authorize(&self, p: &AuthorizeParams) -> Result<(OAuthClient, Vec<String>, String), OAuthError> {
        if p.response_type != "code" {
            return Err(OAuthError::new(400, "unsupported_response_type", "response_type must be code"));
        }
        let client = self.resolve_client(&p.client_id).await?;
        if !redirect_matches(&client.redirect_uris, &p.redirect_uri) {
            return Err(OAuthError::invalid_request("redirect_uri is not registered for this client"));
        }
        match (&p.code_challenge, p.code_challenge_method.as_deref()) {
            (Some(c), Some("S256")) if c.len() >= 43 => {}
            _ => return Err(OAuthError::invalid_request("PKCE with code_challenge_method=S256 is required")),
        }
        let resource = p.resource.clone().unwrap_or_else(|| self.cfg.mcp_resource());
        if !self.cfg.resources.contains(&resource) {
            return Err(OAuthError::new(400, "invalid_target", "resource is not served by this authorization server"));
        }
        let requested = scopes::parse(p.scope.as_deref().unwrap_or("tools:read tools:call"));
        Ok((client, requested, resource))
    }

    /// After the human approves: store a code and build the redirect.
    pub async fn issue_code(&self, p: &AuthorizeParams, user_id: &str, org_id: &str, project_id: &str, granted: Vec<String>) -> Result<String, OAuthError> {
        let (_, _, resource) = self.validate_authorize(p).await?;
        let code = random_token();
        self.store
            .insert_code(OAuthCode {
                code_hash: hash_token(&code),
                client_id: p.client_id.clone(),
                user_id: user_id.into(),
                org_id: org_id.into(),
                project_id: project_id.into(),
                scopes: granted,
                redirect_uri: p.redirect_uri.clone(),
                code_challenge: p.code_challenge.clone().unwrap_or_default(),
                resource,
                expires_at: Utc::now() + self.cfg.code_ttl,
            })
            .await
            .map_err(OAuthError::server)?;
        let mut u = url::Url::parse(&p.redirect_uri).map_err(|_| OAuthError::invalid_request("redirect_uri"))?;
        u.query_pairs_mut().append_pair("code", &code);
        if let Some(s) = &p.state {
            u.query_pairs_mut().append_pair("state", s);
        }
        u.query_pairs_mut().append_pair("iss", &self.cfg.issuer);
        Ok(u.to_string())
    }

    /// The error redirect for a denied consent.
    pub fn denied_redirect(&self, p: &AuthorizeParams) -> String {
        let mut u = url::Url::parse(&p.redirect_uri).unwrap_or_else(|_| url::Url::parse("http://127.0.0.1/").expect("static url"));
        u.query_pairs_mut().append_pair("error", "access_denied");
        if let Some(s) = &p.state {
            u.query_pairs_mut().append_pair("state", s);
        }
        u.to_string()
    }

    async fn issue_tokens(
        &self,
        family_id: &str,
        client_id: &str,
        user_id: &str,
        org_id: &str,
        project_id: &str,
        granted: &[String],
        resource: &str,
    ) -> Result<TokenResponse, OAuthError> {
        let now = Utc::now();
        let refresh = random_token();
        self.store
            .insert_grant(OAuthGrant {
                id: new_id("grt"),
                family_id: family_id.into(),
                client_id: client_id.into(),
                user_id: user_id.into(),
                org_id: org_id.into(),
                project_id: project_id.into(),
                scopes: granted.to_vec(),
                refresh_hash: hash_token(&refresh),
                resource: resource.into(),
                expires_at: now + self.cfg.refresh_ttl,
                used_at: None,
                created_at: now,
            })
            .await
            .map_err(OAuthError::server)?;
        let scope = granted.join(" ");
        let claims = AccessClaims {
            iss: self.cfg.issuer.clone(),
            sub: user_id.into(),
            aud: resource.into(),
            exp: (now + self.cfg.access_ttl).timestamp(),
            iat: now.timestamp(),
            jti: new_id("at"),
            org: org_id.into(),
            prj: project_id.into(),
            scope: scope.clone(),
            fam: family_id.into(),
            cid: client_id.into(),
        };
        Ok(TokenResponse { access_token: self.signer.sign(&claims), token_type: "Bearer".into(), expires_in: self.cfg.access_ttl.num_seconds(), refresh_token: refresh, scope })
    }

    /// The token endpoint.
    pub async fn token(&self, req: &TokenRequest) -> Result<TokenResponse, OAuthError> {
        match req.grant_type.as_str() {
            "authorization_code" => self.exchange_code(req).await,
            "refresh_token" => self.refresh(req).await,
            DEVICE_GRANT => self.device_poll(req).await,
            other => Err(OAuthError::new(400, "unsupported_grant_type", format!("{other} is not supported"))),
        }
    }

    async fn exchange_code(&self, req: &TokenRequest) -> Result<TokenResponse, OAuthError> {
        let code = req.code.as_deref().ok_or_else(|| OAuthError::invalid_request("code is required"))?;
        let verifier = req.code_verifier.as_deref().ok_or_else(|| OAuthError::invalid_request("code_verifier is required"))?;
        let c = self.store.take_code(&hash_token(code)).await.map_err(OAuthError::server)?.ok_or_else(|| OAuthError::invalid_grant("unknown or used code"))?;
        if c.expires_at <= Utc::now() {
            return Err(OAuthError::invalid_grant("code expired"));
        }
        if req.client_id.as_deref() != Some(c.client_id.as_str()) {
            return Err(OAuthError::invalid_grant("code was issued to another client"));
        }
        if req.redirect_uri.as_deref() != Some(c.redirect_uri.as_str()) {
            return Err(OAuthError::invalid_grant("redirect_uri mismatch"));
        }
        if !pkce_s256_matches(verifier, &c.code_challenge) {
            return Err(OAuthError::invalid_grant("PKCE verification failed"));
        }
        if let Some(r) = &req.resource {
            if r != &c.resource {
                return Err(OAuthError::new(400, "invalid_target", "resource differs from the authorization request"));
            }
        }
        self.issue_tokens(&new_id("fam"), &c.client_id, &c.user_id, &c.org_id, &c.project_id, &c.scopes, &c.resource).await
    }

    async fn refresh(&self, req: &TokenRequest) -> Result<TokenResponse, OAuthError> {
        let rt = req.refresh_token.as_deref().ok_or_else(|| OAuthError::invalid_request("refresh_token is required"))?;
        let g = self.store.find_grant_by_refresh(&hash_token(rt)).await.map_err(OAuthError::server)?.ok_or_else(|| OAuthError::invalid_grant("unknown refresh token"))?;
        if self.store.family_revoked(&g.family_id).await.map_err(OAuthError::server)? {
            return Err(OAuthError::invalid_grant("grant revoked"));
        }
        if req.client_id.as_deref() != Some(g.client_id.as_str()) {
            return Err(OAuthError::invalid_grant("refresh token was issued to another client"));
        }
        if !self.store.consume_grant(&g.id).await.map_err(OAuthError::server)? {
            // Reuse of a rotated refresh token: assume theft, kill the family.
            self.store.revoke_family(&g.family_id).await.map_err(OAuthError::server)?;
            return Err(OAuthError::invalid_grant("refresh token reuse detected; all tokens for this grant are revoked"));
        }
        if g.expires_at <= Utc::now() {
            return Err(OAuthError::invalid_grant("refresh token expired"));
        }
        self.issue_tokens(&g.family_id, &g.client_id, &g.user_id, &g.org_id, &g.project_id, &g.scopes, &g.resource).await
    }

    /// Device authorization request (RFC 8628).
    pub async fn device_start(&self, client_id: &str, scope: Option<&str>) -> Result<(DeviceStart, String), OAuthError> {
        self.resolve_client(client_id).await?;
        let device_code = random_token();
        let code = user_code();
        let requested = scopes::parse(scope.unwrap_or("tools:read tools:call"));
        let interval = 5;
        self.store
            .insert_device(DeviceCode {
                device_hash: hash_token(&device_code),
                user_code: code.clone(),
                client_id: client_id.into(),
                scopes: requested,
                status: "pending".into(),
                user_id: None,
                org_id: None,
                project_id: None,
                expires_at: Utc::now() + self.cfg.device_ttl,
                last_poll_at: None,
                interval_s: interval,
            })
            .await
            .map_err(OAuthError::server)?;
        let verification_uri = format!("{}/device", self.cfg.issuer);
        Ok((
            DeviceStart {
                verification_uri_complete: format!("{verification_uri}?user_code={code}"),
                verification_uri,
                user_code: code.clone(),
                device_code,
                expires_in: self.cfg.device_ttl.num_seconds(),
                interval: interval as i64,
            },
            code,
        ))
    }

    /// The human approved or denied `user_code` at `/device`.
    pub async fn device_decide(&self, user_code: &str, user_id: &str, org_id: &str, project_id: &str, granted: Vec<String>, approve: bool) -> Result<(), OAuthError> {
        let mut d = self
            .store
            .get_device_by_user_code(&user_code.trim().to_uppercase())
            .await
            .map_err(OAuthError::server)?
            .ok_or_else(|| OAuthError::invalid_request("unknown code"))?;
        if d.status != "pending" || d.expires_at <= Utc::now() {
            return Err(OAuthError::invalid_request("this code is no longer valid"));
        }
        d.status = if approve { "approved".into() } else { "denied".into() };
        d.user_id = Some(user_id.into());
        d.org_id = Some(org_id.into());
        d.project_id = Some(project_id.into());
        if approve {
            d.scopes = granted;
        }
        self.store.update_device(d).await.map_err(OAuthError::server)
    }

    pub async fn device_pending(&self, user_code: &str) -> Result<DeviceCode, OAuthError> {
        let d = self
            .store
            .get_device_by_user_code(&user_code.trim().to_uppercase())
            .await
            .map_err(OAuthError::server)?
            .ok_or_else(|| OAuthError::invalid_request("unknown code"))?;
        if d.status != "pending" || d.expires_at <= Utc::now() {
            return Err(OAuthError::invalid_request("this code is no longer valid"));
        }
        Ok(d)
    }

    async fn device_poll(&self, req: &TokenRequest) -> Result<TokenResponse, OAuthError> {
        let dc = req.device_code.as_deref().ok_or_else(|| OAuthError::invalid_request("device_code is required"))?;
        let mut d = self.store.get_device(&hash_token(dc)).await.map_err(OAuthError::server)?.ok_or_else(|| OAuthError::invalid_grant("unknown device_code"))?;
        if req.client_id.as_deref() != Some(d.client_id.as_str()) {
            return Err(OAuthError::invalid_grant("device_code was issued to another client"));
        }
        let now = Utc::now();
        if d.expires_at <= now {
            return Err(OAuthError::new(400, "expired_token", "the device code expired; run login again"));
        }
        match d.status.as_str() {
            "pending" => {
                let too_fast = d.last_poll_at.map(|t| now - t < Duration::seconds(d.interval_s as i64)).unwrap_or(false);
                d.last_poll_at = Some(now);
                self.store.update_device(d).await.map_err(OAuthError::server)?;
                if too_fast {
                    Err(OAuthError::new(400, "slow_down", "polling too fast"))
                } else {
                    Err(OAuthError::new(400, "authorization_pending", "waiting for approval"))
                }
            }
            "denied" => Err(OAuthError::new(400, "access_denied", "the request was denied")),
            "approved" => {
                let (user, org, project) = (d.user_id.clone().unwrap_or_default(), d.org_id.clone().unwrap_or_default(), d.project_id.clone().unwrap_or_default());
                let granted = d.scopes.clone();
                let client = d.client_id.clone();
                d.status = "used".into();
                self.store.update_device(d).await.map_err(OAuthError::server)?;
                let resource = req.resource.clone().unwrap_or_else(|| self.cfg.issuer.clone());
                if !self.cfg.resources.contains(&resource) {
                    return Err(OAuthError::new(400, "invalid_target", "unknown resource"));
                }
                self.issue_tokens(&new_id("fam"), &client, &user, &org, &project, &granted, &resource).await
            }
            _ => Err(OAuthError::invalid_grant("device code already used")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use switchyard_store::MemStore;

    fn server() -> AuthServer {
        AuthServer {
            store: Arc::new(MemStore::new()),
            signer: Arc::new(TokenSigner::new(None).unwrap()),
            cfg: OAuthConfig::for_issuer("https://sy.test"),
            http: None,
        }
    }

    fn verifier() -> String {
        "v".repeat(10) + &random_token()
    }

    fn challenge(v: &str) -> String {
        B64.encode(Sha256::digest(v.as_bytes()))
    }

    async fn authorized(s: &AuthServer) -> (String, AuthorizeParams, String) {
        let c = s.register(&json!({"redirect_uris": ["http://127.0.0.1:33418/callback"], "client_name": "Claude Code"})).await.unwrap();
        let v = verifier();
        let p = AuthorizeParams {
            response_type: "code".into(),
            client_id: c.client_id.clone(),
            redirect_uri: "http://127.0.0.1:40000/callback".into(),
            scope: Some("tools:read tools:call tools:send".into()),
            state: Some("xyz".into()),
            code_challenge: Some(challenge(&v)),
            code_challenge_method: Some("S256".into()),
            resource: Some("https://sy.test/mcp".into()),
        };
        let (_, requested, _) = s.validate_authorize(&p).await.unwrap();
        assert_eq!(requested.len(), 3);
        let redirect = s.issue_code(&p, "u1", "org_1", "prj_1", vec!["tools:call".into(), "tools:read".into()]).await.unwrap();
        let u = url::Url::parse(&redirect).unwrap();
        let code = u.query_pairs().find(|(k, _)| k == "code").unwrap().1.to_string();
        assert_eq!(u.query_pairs().find(|(k, _)| k == "state").unwrap().1, "xyz");
        (code, p, v)
    }

    #[test]
    fn pkce_s256() {
        let v = verifier();
        assert!(pkce_s256_matches(&v, &challenge(&v)));
        assert!(!pkce_s256_matches(&v, &challenge("other-verifier-other-verifier-other-verifier")));
        assert!(!pkce_s256_matches("short", &challenge("short")), "verifier length enforced");
    }

    #[tokio::test]
    async fn code_flow_with_pkce_and_single_use_code() {
        let s = server();
        let (code, p, v) = authorized(&s).await;
        let req = TokenRequest {
            grant_type: "authorization_code".into(),
            code: Some(code.clone()),
            redirect_uri: Some(p.redirect_uri.clone()),
            code_verifier: Some("wrong".repeat(10)),
            client_id: Some(p.client_id.clone()),
            ..Default::default()
        };
        assert_eq!(s.token(&req).await.unwrap_err().error, "invalid_grant", "bad verifier");
        // The code was consumed by the failed attempt too (single use).
        let (code, p, v2) = authorized(&s).await;
        let _ = v;
        let ok = s
            .token(&TokenRequest { code: Some(code.clone()), code_verifier: Some(v2.clone()), client_id: Some(p.client_id.clone()), redirect_uri: Some(p.redirect_uri.clone()), ..req.clone() })
            .await
            .unwrap();
        let claims = s.signer.verify(&ok.access_token, &["https://sy.test/mcp"], Utc::now().timestamp()).unwrap();
        assert_eq!((claims.prj.as_str(), claims.scope.as_str()), ("prj_1", "tools:call tools:read"));
        assert!(s.signer.verify(&ok.access_token, &["https://sy.test"], Utc::now().timestamp()).is_err(), "bound to the MCP resource");
        let again = s.token(&TokenRequest { code: Some(code), code_verifier: Some(v2), client_id: Some(p.client_id.clone()), redirect_uri: Some(p.redirect_uri), ..req }).await;
        assert!(again.is_err(), "codes are single use");
    }

    #[tokio::test]
    async fn authorize_rejects_bad_requests() {
        let s = server();
        let c = s.register(&json!({"redirect_uris": ["https://app.example/cb"]})).await.unwrap();
        let base = AuthorizeParams {
            response_type: "code".into(),
            client_id: c.client_id.clone(),
            redirect_uri: "https://app.example/cb".into(),
            scope: None,
            state: None,
            code_challenge: Some(challenge(&verifier())),
            code_challenge_method: Some("S256".into()),
            resource: None,
        };
        assert!(s.validate_authorize(&base).await.is_ok());
        let plain = AuthorizeParams { code_challenge_method: Some("plain".into()), ..base.clone() };
        assert!(s.validate_authorize(&plain).await.is_err(), "S256 only");
        let other_redirect = AuthorizeParams { redirect_uri: "https://evil.example/cb".into(), ..base.clone() };
        assert!(s.validate_authorize(&other_redirect).await.is_err());
        let wrong_resource = AuthorizeParams { resource: Some("https://other.example/mcp".into()), ..base.clone() };
        assert_eq!(s.validate_authorize(&wrong_resource).await.unwrap_err().error, "invalid_target");
        assert!(s.register(&json!({"redirect_uris": ["http://evil.example/cb"]})).await.is_err(), "non-loopback http refused");
        assert!(s.register(&json!({"redirect_uris": ["https://a/cb"], "token_endpoint_auth_method": "client_secret_basic"})).await.is_err());
    }

    #[tokio::test]
    async fn refresh_rotation_and_replay_revokes_family() {
        let s = server();
        let (code, p, v) = authorized(&s).await;
        let t1 = s
            .token(&TokenRequest { grant_type: "authorization_code".into(), code: Some(code), code_verifier: Some(v), client_id: Some(p.client_id.clone()), redirect_uri: Some(p.redirect_uri.clone()), ..Default::default() })
            .await
            .unwrap();
        let r = |rt: &str| TokenRequest { grant_type: "refresh_token".into(), refresh_token: Some(rt.into()), client_id: Some(p.client_id.clone()), ..Default::default() };
        let t2 = s.token(&r(&t1.refresh_token)).await.unwrap();
        assert_ne!(t2.refresh_token, t1.refresh_token, "rotates");
        // Attacker replays the old refresh token.
        let e = s.token(&r(&t1.refresh_token)).await.unwrap_err();
        assert!(e.error_description.contains("reuse"));
        // The legitimate newer token is now dead too.
        assert_eq!(s.token(&r(&t2.refresh_token)).await.unwrap_err().error_description, "grant revoked");
    }

    #[tokio::test]
    async fn device_flow() {
        let s = server();
        let (start, user_code) = s.device_start(CLI_CLIENT_ID, Some("tools:read tools:call")).await.unwrap();
        assert_eq!(user_code.len(), 9);
        let poll = TokenRequest { grant_type: DEVICE_GRANT.into(), device_code: Some(start.device_code.clone()), client_id: Some(CLI_CLIENT_ID.into()), ..Default::default() };
        assert_eq!(s.token(&poll).await.unwrap_err().error, "authorization_pending");
        assert_eq!(s.token(&poll).await.unwrap_err().error, "slow_down");
        s.device_decide(&user_code.to_lowercase(), "u1", "org_1", "prj_1", vec!["tools:read".into()], true).await.unwrap();
        let t = s.token(&poll).await.unwrap();
        let c = s.signer.verify(&t.access_token, &["https://sy.test"], Utc::now().timestamp()).unwrap();
        assert_eq!((c.sub.as_str(), c.scope.as_str(), c.cid.as_str()), ("u1", "tools:read", CLI_CLIENT_ID));
        assert!(s.token(&poll).await.is_err(), "device codes are single use");
        let (start2, code2) = s.device_start(CLI_CLIENT_ID, None).await.unwrap();
        s.device_decide(&code2, "u1", "org_1", "prj_1", vec![], false).await.unwrap();
        let denied = TokenRequest { device_code: Some(start2.device_code), ..poll };
        assert_eq!(s.token(&denied).await.unwrap_err().error, "access_denied");
    }

    struct FakeCimd;
    #[async_trait::async_trait]
    impl HttpTransport for FakeCimd {
        async fn send(&self, req: HttpRequest) -> Result<switchyard_core::HttpResponse, switchyard_core::EgressError> {
            let body = if req.url.contains("good") {
                json!({"client_id": req.url, "client_name": "Good", "redirect_uris": ["http://127.0.0.1/cb"]})
            } else {
                json!({"client_id": "https://someone-else/meta.json", "redirect_uris": ["http://127.0.0.1/cb"]})
            };
            Ok(switchyard_core::HttpResponse { status: 200, headers: vec![], body: body.to_string().into_bytes(), final_url: req.url })
        }
    }

    #[tokio::test]
    async fn client_id_metadata_documents() {
        let mut s = server();
        s.http = Some(Arc::new(FakeCimd));
        let c = s.resolve_client("https://good.example/oauth/client.json").await.unwrap();
        assert_eq!(c.kind, "cimd");
        assert_eq!(c.redirect_uris, vec!["http://127.0.0.1/cb".to_string()]);
        assert!(s.resolve_client("https://bad.example/oauth/client.json").await.is_err(), "client_id must match URL");
        assert!(s.resolve_client("https://good.example/").await.is_err(), "needs a path");
    }
}
