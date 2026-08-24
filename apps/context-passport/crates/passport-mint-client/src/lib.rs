//! Real HTTP client to Mint, implementing `MintMeter`.
//!
//! Meters a passport-conditioned generation on the Mint ledger, tagging the
//! settlement with the passport id + version so Mint receipts and Ledgerline
//! margins are attributable per character/brand/persona. Success-only (ADR-0002);
//! the reference is the generation id, making the call idempotent on retry.

use async_trait::async_trait;

use passport_core::{MintMeter, MintMeterRequest, MintMeterResult, PassportError};

#[derive(Clone)]
pub struct MintClient {
    http: reqwest::Client,
    base: String,
    api_key: String,
    path: String,
}

impl MintClient {
    pub fn new(base: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            path: "/v1/meter".to_string(),
        }
    }

    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }
}

#[async_trait]
impl MintMeter for MintClient {
    async fn meter(&self, request: &MintMeterRequest) -> Result<MintMeterResult, PassportError> {
        let response = self
            .http
            .post(format!("{}{}", self.base, self.path))
            .header("X-API-Key", &self.api_key)
            .json(request)
            .send()
            .await
            .map_err(|e| PassportError::Mint(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(PassportError::Mint(format!(
                "mint returned {status}: {body}"
            )));
        }

        response
            .json::<MintMeterResult>()
            .await
            .map_err(|e| PassportError::Mint(format!("decoding mint response: {e}")))
    }
}
