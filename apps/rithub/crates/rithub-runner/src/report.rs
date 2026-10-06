//! Reporting check runs to the forge.

use async_trait::async_trait;
use serde_json::{json, Value};

#[derive(Debug, thiserror::Error)]
#[error("reporting to the forge failed: {0}")]
pub struct ReportError(pub String);

/// Where check runs go.
#[async_trait]
pub trait Reporter: Send + Sync {
    /// Open an `in_progress` check run; returns its id.
    async fn start(&self, name: &str, head_sha: &str) -> Result<String, ReportError>;
    /// Complete it with a conclusion and a summary.
    async fn finish(&self, id: &str, conclusion: &str, title: &str, summary: &str) -> Result<(), ReportError>;
}

/// The forge's runner report API, authenticated with the runner's token.
#[derive(Clone)]
pub struct HttpReporter {
    pub client: reqwest::Client,
    /// `https://forge.example/api/orgs/{org}/repos/{repo}`.
    pub repo_api: String,
    pub token: String,
}

impl std::fmt::Debug for HttpReporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpReporter").field("repo_api", &self.repo_api).finish_non_exhaustive()
    }
}

impl HttpReporter {
    pub fn new(base_url: &str, org: &str, repo: &str, token: impl Into<String>) -> Self {
        Self {
            client: reqwest::Client::new(),
            repo_api: format!("{}/api/orgs/{org}/repos/{repo}", base_url.trim_end_matches('/')),
            token: token.into(),
        }
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Result<Value, ReportError> {
        let resp = req.bearer_auth(&self.token).send().await.map_err(|e| ReportError(e.to_string()))?;
        let status = resp.status();
        let body = resp.text().await.map_err(|e| ReportError(e.to_string()))?;
        if !status.is_success() {
            return Err(ReportError(format!("{status}: {}", body.chars().take(300).collect::<String>())));
        }
        serde_json::from_str(&body).map_err(|e| ReportError(e.to_string()))
    }
}

/// The forge caps summaries; keep the head of a long one.
const MAX_SUMMARY: usize = 60_000;

#[async_trait]
impl Reporter for HttpReporter {
    async fn start(&self, name: &str, head_sha: &str) -> Result<String, ReportError> {
        let body = json!({ "name": name, "head_sha": head_sha, "status": "in_progress" });
        let v = self.send(self.client.post(format!("{}/check-runs", self.repo_api)).json(&body)).await?;
        v.get("id").and_then(Value::as_str).map(str::to_string).ok_or_else(|| ReportError("no id in the check run".into()))
    }

    async fn finish(&self, id: &str, conclusion: &str, title: &str, summary: &str) -> Result<(), ReportError> {
        let summary: String = summary.chars().take(MAX_SUMMARY).collect();
        let body = json!({ "status": "completed", "conclusion": conclusion, "output_title": title, "output_summary": summary });
        self.send(self.client.patch(format!("{}/check-runs/{id}", self.repo_api)).json(&body)).await.map(|_| ())
    }
}
