//! Rago usage events (`code = tool_call`, `transaction_id = call_id`).

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("rago: {message}")]
pub struct RagoError {
    pub retryable: bool,
    pub message: String,
}

#[async_trait]
pub trait RagoTransport: Send + Sync {
    /// Deliver one event (`{"event": {...}}` body). A duplicate
    /// `transaction_id` counts as delivered.
    async fn send_event(&self, event: &Value) -> Result<(), RagoError>;
}

#[derive(Clone)]
pub struct RagoClient {
    http: reqwest::Client,
    base: String,
    api_key: String,
}

impl RagoClient {
    pub fn new(base: &str, api_key: &str) -> Self {
        let http = reqwest::Client::builder().timeout(Duration::from_secs(10)).build().expect("reqwest client");
        RagoClient { http, base: base.trim_end_matches('/').to_string(), api_key: api_key.to_string() }
    }
}

#[async_trait]
impl RagoTransport for RagoClient {
    async fn send_event(&self, event: &Value) -> Result<(), RagoError> {
        let resp = self
            .http
            .post(format!("{}/api/v1/events", self.base))
            .bearer_auth(&self.api_key)
            .json(&serde_json::json!({ "event": event }))
            .send()
            .await
            .map_err(|e| RagoError { retryable: true, message: e.to_string() })?;
        let status = resp.status().as_u16();
        if (200..300).contains(&status) {
            return Ok(());
        }
        let body = resp.text().await.unwrap_or_default();
        // Lago-compatible APIs answer 422 `transaction_id` when the event was
        // already ingested: that is a successful redelivery.
        if status == 422 && body.contains("transaction_id") {
            return Ok(());
        }
        Err(RagoError { retryable: status == 429 || status >= 500, message: format!("{status}: {}", body.chars().take(300).collect::<String>()) })
    }
}
