//! HTTP client with stored-token refresh.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CLIENT_ID: &str = "switchyard-cli";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Stored {
    pub url: String,
    pub access_token: String,
    pub refresh_token: String,
    /// Unix seconds.
    pub expires_at: i64,
}

impl Stored {
    pub fn path(dir: &Path) -> PathBuf {
        dir.join("credentials.json")
    }

    pub fn load(dir: &Path) -> Option<Stored> {
        std::fs::read(Self::path(dir)).ok().and_then(|b| serde_json::from_slice(&b).ok())
    }

    /// Write with owner-only permissions.
    pub fn save(&self, dir: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(dir)?;
        let p = Self::path(dir);
        std::fs::write(&p, serde_json::to_vec_pretty(self)?)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}

pub struct Client {
    pub base: String,
    http: reqwest::Client,
    token: String,
    stored: Option<Stored>,
    dir: PathBuf,
}

#[derive(Debug)]
pub struct Reply {
    pub status: u16,
    pub body: Value,
    pub text: String,
}

impl Client {
    pub fn http() -> reqwest::Client {
        reqwest::Client::builder().user_agent(concat!("switchyard-cli/", env!("CARGO_PKG_VERSION"))).build().expect("reqwest")
    }

    /// `SWITCHYARD_TOKEN` (+ `SWITCHYARD_URL`) wins; otherwise the stored login.
    pub fn from_env(dir: &Path, env_token: Option<String>, env_url: Option<String>) -> anyhow::Result<Self> {
        if let Some(t) = env_token {
            let base = env_url.context("SWITCHYARD_URL must be set with SWITCHYARD_TOKEN")?;
            return Ok(Client { base: base.trim_end_matches('/').into(), http: Self::http(), token: t, stored: None, dir: dir.to_path_buf() });
        }
        let s = Stored::load(dir).context("not signed in: run `sy login --url https://your-switchyard`")?;
        Ok(Client { base: s.url.clone(), http: Self::http(), token: s.access_token.clone(), stored: Some(s), dir: dir.to_path_buf() })
    }

    async fn refresh_if_needed(&mut self) -> anyhow::Result<()> {
        let Some(s) = &self.stored else { return Ok(()) };
        if s.expires_at > chrono::Utc::now().timestamp() + 30 {
            return Ok(());
        }
        let resp = self
            .http
            .post(format!("{}/oauth/token", s.url))
            .form(&[("grant_type", "refresh_token"), ("refresh_token", s.refresh_token.as_str()), ("client_id", CLIENT_ID)])
            .send()
            .await?;
        let v: Value = resp.json().await?;
        let Some(at) = v.get("access_token").and_then(Value::as_str) else {
            bail!("session expired ({}); run `sy login` again", v.get("error_description").and_then(Value::as_str).unwrap_or("refresh failed"));
        };
        let ns = Stored {
            url: s.url.clone(),
            access_token: at.into(),
            refresh_token: v.get("refresh_token").and_then(Value::as_str).unwrap_or_default().into(),
            expires_at: chrono::Utc::now().timestamp() + v.get("expires_in").and_then(Value::as_i64).unwrap_or(600),
        };
        ns.save(&self.dir)?;
        self.token = ns.access_token.clone();
        self.stored = Some(ns);
        Ok(())
    }

    pub async fn request(&mut self, method: reqwest::Method, path: &str, body: Option<&Value>, headers: &[(&str, String)]) -> anyhow::Result<Reply> {
        self.refresh_if_needed().await?;
        let mut rb = self.http.request(method, format!("{}{path}", self.base)).bearer_auth(&self.token);
        for (k, v) in headers {
            rb = rb.header(*k, v);
        }
        if let Some(b) = body {
            rb = rb.json(b);
        }
        let resp = rb.send().await.with_context(|| format!("cannot reach {}", self.base))?;
        let status = resp.status().as_u16();
        let text = resp.text().await?;
        let body = serde_json::from_str(&text).unwrap_or(Value::Null);
        Ok(Reply { status, body, text })
    }

    pub async fn get(&mut self, path: &str) -> anyhow::Result<Reply> {
        self.request(reqwest::Method::GET, path, None, &[]).await
    }

    pub async fn post(&mut self, path: &str, body: &Value) -> anyhow::Result<Reply> {
        self.request(reqwest::Method::POST, path, Some(body), &[]).await
    }

    /// Raw MCP message (the stdio shim).
    pub async fn mcp(&mut self, msg: &str) -> anyhow::Result<Option<String>> {
        self.refresh_if_needed().await?;
        let resp = self
            .http
            .post(format!("{}/mcp", self.base))
            .bearer_auth(&self.token)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .body(msg.to_string())
            .send()
            .await?;
        if resp.status().as_u16() == 202 {
            return Ok(None);
        }
        Ok(Some(resp.text().await?))
    }
}
