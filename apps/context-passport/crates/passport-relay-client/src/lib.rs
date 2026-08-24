//! Real HTTP client to the Relay gateway, implementing `RelayConditioningSink`.
//!
//! Relay (a TensorZero fork) performs the provider fan-out; the Passport supplies
//! the per-provider conditioning. This client submits a resolved
//! `RelayGenerationRequest` and returns the produced asset reference. The exact
//! Relay conditioning endpoint contract is refined in Sx1; the transport,
//! auth, and error mapping are real now and faked in tests via `FakeRelay`.

use async_trait::async_trait;

use passport_core::{
    PassportError, RelayConditioningSink, RelayGenerationRequest, RelayGenerationResult,
};

#[derive(Clone)]
pub struct RelayClient {
    http: reqwest::Client,
    base: String,
    api_key: String,
    /// Path the conditioned-generation request is POSTed to.
    path: String,
}

impl RelayClient {
    pub fn new(base: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            path: "/v1/passport/generate".to_string(),
        }
    }

    /// Override the Relay generation path (defaults to `/v1/passport/generate`).
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }
}

#[async_trait]
impl RelayConditioningSink for RelayClient {
    async fn generate(
        &self,
        request: &RelayGenerationRequest,
    ) -> Result<RelayGenerationResult, PassportError> {
        let response = self
            .http
            .post(format!("{}{}", self.base, self.path))
            .header("X-API-Key", &self.api_key)
            .json(request)
            .send()
            .await
            .map_err(|e| PassportError::Relay(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(PassportError::Relay(format!(
                "relay returned {status}: {body}"
            )));
        }

        response
            .json::<RelayGenerationResult>()
            .await
            .map_err(|e| PassportError::Relay(format!("decoding relay response: {e}")))
    }
}
