//! Who the human at the consent screen is. Sign-in reuses Scribase auth: the
//! consent and device pages accept a Scribase session and resolve it through
//! Scribase's `/v1/account`, so Switchyard builds no login of its own.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use switchyard_core::ApiError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct User {
    pub id: String,
    pub email: String,
}

#[async_trait]
pub trait IdentityProvider: Send + Sync {
    async fn user_from_session(&self, session_token: &str) -> Result<User, ApiError>;
    /// Where to send a browser without a session (`return_to` comes back after sign-in).
    fn login_url(&self, return_to: &str) -> Option<String>;
}

pub struct ScribaseIdentity {
    http: reqwest::Client,
    api_base: String,
    console_login: Option<String>,
}

impl ScribaseIdentity {
    pub fn new(api_base: &str, console_login: Option<&str>) -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().expect("reqwest");
        ScribaseIdentity { http, api_base: api_base.trim_end_matches('/').into(), console_login: console_login.map(str::to_string) }
    }
}

#[async_trait]
impl IdentityProvider for ScribaseIdentity {
    async fn user_from_session(&self, session_token: &str) -> Result<User, ApiError> {
        let resp = self
            .http
            .get(format!("{}/v1/account", self.api_base))
            .bearer_auth(session_token)
            .send()
            .await
            .map_err(|e| ApiError::new(503, "identity_unavailable", e.to_string()))?;
        if !resp.status().is_success() {
            return Err(ApiError::unauthorized("Scribase session is not valid; sign in again"));
        }
        let v: Value = resp.json().await.map_err(|e| ApiError::new(502, "identity_bad_response", e.to_string()))?;
        let v = v.get("data").cloned().unwrap_or(v);
        let id = ["id", "user_id", "subject", "sub"].iter().find_map(|k| v.get(*k).and_then(Value::as_str)).unwrap_or_default();
        let email = v.get("email").and_then(Value::as_str).unwrap_or_default();
        if id.is_empty() {
            return Err(ApiError::new(502, "identity_bad_response", "Scribase account has no id"));
        }
        Ok(User { id: id.into(), email: email.into() })
    }

    fn login_url(&self, return_to: &str) -> Option<String> {
        self.console_login.as_ref().map(|l| {
            let enc: String = url::form_urlencoded::byte_serialize(return_to.as_bytes()).collect();
            format!("{l}?next={enc}")
        })
    }
}

/// Fixed session tokens (tests; `SWITCHYARD_DEV_SESSIONS=token:user:email,...` for local dev).
#[derive(Default)]
pub struct StaticIdentity {
    sessions: HashMap<String, User>,
}

impl StaticIdentity {
    pub fn new(entries: &[(&str, &str, &str)]) -> Self {
        StaticIdentity { sessions: entries.iter().map(|(t, id, email)| (t.to_string(), User { id: id.to_string(), email: email.to_string() })).collect() }
    }
    pub fn parse(spec: &str) -> Self {
        let mut s = StaticIdentity::default();
        for e in spec.split(',').filter(|e| !e.trim().is_empty()) {
            let parts: Vec<&str> = e.trim().splitn(3, ':').collect();
            if parts.len() == 3 {
                s.sessions.insert(parts[0].into(), User { id: parts[1].into(), email: parts[2].into() });
            }
        }
        s
    }
}

#[async_trait]
impl IdentityProvider for StaticIdentity {
    async fn user_from_session(&self, session_token: &str) -> Result<User, ApiError> {
        self.sessions.get(session_token).cloned().ok_or_else(|| ApiError::unauthorized("unknown session"))
    }
    fn login_url(&self, _return_to: &str) -> Option<String> {
        None
    }
}
