//! Default LLM client (behind the `llm` feature).
//!
//! A thin wrapper over the Anthropic Messages API using blocking `reqwest`. It is
//! the only place a real network call lives; the [`Ask`](super::Ask) seam keeps
//! the default build and every test free of any API dependency. Enable with
//! `--features llm` and set `ANTHROPIC_API_KEY`.

use super::{parse_llm_json, Ask, LlmResponse};

/// Anthropic-backed [`Ask`]. Model defaults to `claude-opus-4-8`; override with
/// `AEONRIFT_LLM_MODEL`.
pub struct DefaultAsk {
    api_key: String,
    model: String,
}

impl DefaultAsk {
    /// Build from the environment, or explain why it is unavailable.
    pub fn from_env() -> Result<Self, String> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .map_err(|_| "ANTHROPIC_API_KEY is not set".to_string())?;
        let model = std::env::var("AEONRIFT_LLM_MODEL")
            .unwrap_or_else(|_| "claude-opus-4-8".to_string());
        Ok(Self { api_key, model })
    }
}

impl Ask for DefaultAsk {
    fn ask(&self, system: &str, context: &str, question: &str) -> Result<LlmResponse, String> {
        let user = format!(
            "{context}\n\n# QUESTION\n{question}\n\n\
             Answer strictly from the SUBGRAPH above. Reply with a JSON object \
             {{\"answer\": <string>, \"citations\": [<node/edge ids copied verbatim>]}}."
        );
        let body = serde_json::json!({
            "model": self.model,
            "max_tokens": 1024,
            "system": system,
            "messages": [{ "role": "user", "content": user }],
        });

        let client = reqwest::blocking::Client::new();
        let resp = client
            .post("https://api.anthropic.com/v1/messages")
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .map_err(|e| format!("request failed: {e}"))?;

        let status = resp.status();
        let val: serde_json::Value = resp
            .json()
            .map_err(|e| format!("invalid JSON response: {e}"))?;
        if !status.is_success() {
            return Err(format!("anthropic error {status}: {val}"));
        }

        // First text block of the content array.
        let text = val
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|blocks| {
                blocks.iter().find_map(|b| {
                    if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                        b.get("text").and_then(|t| t.as_str())
                    } else {
                        None
                    }
                })
            })
            .unwrap_or("")
            .to_string();

        Ok(parse_llm_json(&text).unwrap_or(LlmResponse {
            answer: text,
            citations: Vec::new(),
        }))
    }
}
