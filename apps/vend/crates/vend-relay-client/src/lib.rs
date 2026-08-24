//! # vend-relay-client — real HTTP client to the live Relay.
//!
//! Implements [`RelayTransport`]: forwards the OpenAI-shaped body with
//! `Authorization: Bearer`, applies the app's Relay observability policy, and
//! parses terminal usage from either a non-streaming JSON body or the final SSE
//! `usage` frame that precedes `data: [DONE]`.

use async_trait::async_trait;
use futures::StreamExt;
use std::fmt;
use std::time::Duration;

use vend_core::error::VendError;
use vend_core::transport::{
    parse_usage, RelayRequest, RelayResponse, RelayStreamResponse, RelayTransport, Usage,
};

/// A configured client for one Relay deployment.
#[derive(Clone)]
pub struct RelayClient {
    http: reqwest::Client,
    base: String,
    request_timeout: Duration,
    stream_timeout: Duration,
}

/// Network deadlines and connection-pool bounds for the Relay dependency.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayClientConfig {
    pub connect_timeout: Duration,
    /// Deadline for a non-streaming completion, including its response body.
    pub request_timeout: Duration,
    /// Maximum silence between response chunks, including streaming SSE chunks.
    pub read_timeout: Duration,
    /// Absolute lifetime for one streaming completion.
    pub stream_timeout: Duration,
    pub pool_idle_timeout: Duration,
    pub max_idle_per_host: usize,
}

impl Default for RelayClientConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(5),
            request_timeout: Duration::from_secs(120),
            read_timeout: Duration::from_secs(45),
            stream_timeout: Duration::from_secs(900),
            pool_idle_timeout: Duration::from_secs(30),
            max_idle_per_host: 64,
        }
    }
}

/// Invalid URL or timeout configuration. It is intentionally secret-free.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayClientConfigError(String);

impl fmt::Display for RelayClientConfigError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for RelayClientConfigError {}

impl RelayClient {
    /// Build a client from a Relay base URL (e.g. `https://relay.internal`).
    pub fn new(base: impl Into<String>) -> Self {
        Self::with_config(base, RelayClientConfig::default())
            .expect("default Relay HTTP client configuration must be valid")
    }

    /// Build a hardened client with explicit network deadlines.
    pub fn with_config(
        base: impl Into<String>,
        config: RelayClientConfig,
    ) -> Result<Self, RelayClientConfigError> {
        validate_config(&config)?;
        let base = normalize_base(base.into())?;
        // Apply different total deadlines per request below. Streaming needs a
        // larger but still finite lifetime so reconciliation can distinguish a
        // live request from an abandoned hold.
        let http = reqwest::Client::builder()
            .connect_timeout(config.connect_timeout)
            .read_timeout(config.read_timeout)
            .pool_idle_timeout(config.pool_idle_timeout)
            .pool_max_idle_per_host(config.max_idle_per_host)
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("vend-relay-client/0.1")
            .build()
            .map_err(|error| RelayClientConfigError(format!("build Relay HTTP client: {error}")))?;
        Ok(RelayClient {
            http,
            base,
            request_timeout: config.request_timeout,
            stream_timeout: config.stream_timeout,
        })
    }
}

fn normalize_base(base: String) -> Result<String, RelayClientConfigError> {
    let base = base.trim_end_matches('/');
    let parsed = reqwest::Url::parse(base)
        .map_err(|_| RelayClientConfigError("Relay base URL must be an absolute URL".into()))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(RelayClientConfigError(
            "Relay base URL must use http or https and include a host".into(),
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(RelayClientConfigError(
            "Relay base URL must not contain credentials".into(),
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(RelayClientConfigError(
            "Relay base URL must not contain a query or fragment".into(),
        ));
    }
    Ok(base.to_string())
}

fn validate_config(config: &RelayClientConfig) -> Result<(), RelayClientConfigError> {
    if config.connect_timeout.is_zero()
        || config.request_timeout.is_zero()
        || config.read_timeout.is_zero()
        || config.stream_timeout.is_zero()
        || config.pool_idle_timeout.is_zero()
        || config.max_idle_per_host == 0
    {
        return Err(RelayClientConfigError(
            "Relay HTTP timeouts and pool limit must be greater than zero".into(),
        ));
    }
    Ok(())
}

/// Relay's OpenAI-compatible API recognizes `tensorzero::dryrun`; when true it
/// executes inference without writing the inference to Relay observability.
/// An app policy always wins over a caller-provided false value.
fn apply_observability_policy(body: &mut serde_json::Value, no_store: bool) {
    if no_store {
        if let Some(object) = body.as_object_mut() {
            object.insert("tensorzero::dryrun".to_string(), true.into());
        }
    }
}

/// Extract terminal usage from a raw Relay response body that may be JSON or an
/// SSE stream. For SSE, scan `data:` frames and take the last non-null `usage`
/// seen before the `[DONE]` sentinel.
pub fn parse_body(status: u16, raw: &str) -> RelayResponse {
    // Non-streaming JSON first.
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
        let usage = parse_usage(&value);
        return RelayResponse {
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
    RelayResponse {
        status,
        body: last_json,
        usage: last_usage,
    }
}

#[async_trait]
impl RelayTransport for RelayClient {
    async fn chat_completion(&self, req: RelayRequest) -> Result<RelayResponse, VendError> {
        let url = format!(
            "{}{}/chat/completions",
            self.base,
            req.base_path.trim_end_matches('/')
        );
        let mut body = req.body;
        apply_observability_policy(&mut body, req.zdr);
        let resp = self
            .http
            .post(url)
            .header("Authorization", format!("Bearer {}", req.bearer))
            .json(&body)
            .timeout(self.request_timeout)
            .send()
            .await
            .map_err(|e| VendError::Transport(e.to_string()))?;

        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| VendError::Transport(e.to_string()))?;
        Ok(parse_body(status, &text))
    }

    async fn chat_completion_stream(
        &self,
        req: RelayRequest,
    ) -> Result<RelayStreamResponse, VendError> {
        let url = format!(
            "{}{}/chat/completions",
            self.base,
            req.base_path.trim_end_matches('/')
        );
        let mut body = req.body;
        apply_observability_policy(&mut body, req.zdr);
        let resp = self
            .http
            .post(url)
            .header("Authorization", format!("Bearer {}", req.bearer))
            .json(&body)
            .timeout(self.stream_timeout)
            .send()
            .await
            .map_err(|error| VendError::Transport(error.to_string()))?;

        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str().to_string(), value.as_bytes().to_vec()))
            .collect();
        let stream = resp.bytes_stream().map(|item| {
            item.map(|bytes| bytes.to_vec())
                .map_err(|error| VendError::Transport(error.to_string()))
        });
        Ok(RelayStreamResponse {
            status,
            headers,
            stream: Box::pin(stream),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hardened_config_rejects_unsafe_or_unbounded_values() {
        assert!(
            RelayClient::with_config("file:///tmp/relay", RelayClientConfig::default()).is_err()
        );
        assert!(RelayClient::with_config(
            "https://user:password@relay.internal",
            RelayClientConfig::default()
        )
        .is_err());
        let invalid = RelayClientConfig {
            read_timeout: Duration::ZERO,
            ..RelayClientConfig::default()
        };
        assert!(RelayClient::with_config("https://relay.internal", invalid).is_err());
    }

    #[tokio::test]
    async fn total_deadline_stops_a_stalled_non_streaming_completion() {
        use axum::routing::post;
        use axum::Router;

        let app = Router::new().route(
            "/openai/v1/chat/completions",
            post(|| async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                axum::Json(serde_json::json!({"choices": []}))
            }),
        );
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("bind timeout test server: {error}"),
        };
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = RelayClient::with_config(
            base,
            RelayClientConfig {
                request_timeout: Duration::from_millis(20),
                ..RelayClientConfig::default()
            },
        )
        .unwrap();
        let error = client
            .chat_completion(RelayRequest {
                base_path: "/openai/v1".into(),
                bearer: "secret".into(),
                body: serde_json::json!({"model": "test"}),
                zdr: true,
            })
            .await
            .unwrap_err();
        assert!(matches!(error, VendError::Transport(_)));
    }

    #[test]
    fn parses_non_streaming_usage() {
        let raw = r#"{"id":"x","usage":{"prompt_tokens":11,"completion_tokens":4}}"#;
        let resp = parse_body(200, raw);
        assert_eq!(
            resp.usage,
            Some(Usage {
                input_tokens: 11,
                output_tokens: 4,
                provider_cost_micros: None,
            })
        );
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
                output_tokens: 3,
                provider_cost_micros: None,
            })
        );
    }

    #[test]
    fn no_store_policy_uses_relays_openai_compatible_dryrun_field() {
        let mut protected = serde_json::json!({"model": "openai::gpt-4o-mini"});
        apply_observability_policy(&mut protected, true);
        assert_eq!(protected["tensorzero::dryrun"], true);

        let mut normal = serde_json::json!({"model": "openai::gpt-4o-mini"});
        apply_observability_policy(&mut normal, false);
        assert!(normal.get("tensorzero::dryrun").is_none());

        let mut caller_override = serde_json::json!({"tensorzero::dryrun": false});
        apply_observability_policy(&mut caller_override, true);
        assert_eq!(caller_override["tensorzero::dryrun"], true);
    }
}
