//! # abacus-upstream — real HTTP forwarder to the provider.
//!
//! Implements [`UpstreamTransport`]: forwards the OpenAI-compatible / tool body
//! verbatim to the configured provider base URL with `Authorization: Bearer`, and
//! parses normalized terminal [`Usage`] from either a non-streaming JSON body or
//! the final SSE `usage` frame preceding `data: [DONE]`. Mirrors
//! `vend-relay-client` so the forwarder is swappable and fake-able independently
//! of Mint.

use async_trait::async_trait;

use abacus_core::error::AbacusError;
use abacus_core::transport::{
    parse_usage, UpstreamRequest, UpstreamResponse, UpstreamTransport, Usage,
};

/// A configured forwarder for one provider deployment.
#[derive(Clone)]
pub struct UpstreamClient {
    http: reqwest::Client,
    base: String,
}

impl UpstreamClient {
    /// Build a forwarder from the provider base URL (e.g. `https://api.openai.com`),
    /// read from `UPSTREAM_BASE_URL` at startup, never a literal.
    pub fn new(base: impl Into<String>) -> Self {
        UpstreamClient {
            http: reqwest::Client::new(),
            base: base.into().trim_end_matches('/').to_string(),
        }
    }
}

/// Extract terminal usage from a raw provider response body that may be JSON or an
/// SSE stream. For SSE, scan `data:` frames and take the last non-null `usage`
/// seen before the `[DONE]` sentinel.
pub fn parse_body(status: u16, raw: &str) -> UpstreamResponse {
    // Non-streaming JSON first.
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
        let usage = parse_usage(&value);
        return UpstreamResponse {
            status,
            body: value,
            usage,
        };
    }

    // SSE: walk frames, remembering the last usage-bearing chunk.
    let mut last_usage: Option<Usage> = None;
    let mut last_json = serde_json::Value::Null;
    for line in raw.lines() {
        let Some(payload) = line.strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim();
        if payload == "[DONE]" {
            break;
        }
        if let Ok(chunk) = serde_json::from_str::<serde_json::Value>(payload) {
            if let Some(u) = parse_usage(&chunk) {
                last_usage = Some(u);
            }
            last_json = chunk;
        }
    }
    UpstreamResponse {
        status,
        body: last_json,
        usage: last_usage,
    }
}

#[async_trait]
impl UpstreamTransport for UpstreamClient {
    async fn send(&self, req: UpstreamRequest) -> Result<UpstreamResponse, AbacusError> {
        let url = format!("{}/{}", self.base, req.path.trim_start_matches('/'));
        let resp = self
            .http
            .post(url)
            .header("Authorization", format!("Bearer {}", req.bearer))
            .json(&req.body)
            .send()
            .await
            .map_err(|e| AbacusError::Transport(e.to_string()))?;

        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| AbacusError::Transport(e.to_string()))?;
        Ok(parse_body(status, &text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_non_streaming_usage() {
        let raw = r#"{"id":"x","usage":{"prompt_tokens":11,"completion_tokens":4}}"#;
        let resp = parse_body(200, raw);
        assert_eq!(
            resp.usage,
            Some(Usage {
                input_tokens: 11,
                output_tokens: 4
            })
        );
        assert!(resp.is_success());
    }

    #[test]
    fn parses_sse_terminal_usage_before_done() {
        let raw = concat!(
            "data: {\"choices\":[{}],\"usage\":null}\n\n",
            "data: {\"choices\":[],\"usage\":{\"input_tokens\":9,\"output_tokens\":3}}\n\n",
            "data: [DONE]\n\n"
        );
        let resp = parse_body(200, raw);
        assert_eq!(
            resp.usage,
            Some(Usage {
                input_tokens: 9,
                output_tokens: 3
            })
        );
    }

    #[test]
    fn non_2xx_is_not_success() {
        let resp = parse_body(500, r#"{"error":"upstream"}"#);
        assert!(!resp.is_success());
        assert_eq!(resp.usage, None);
    }
}
