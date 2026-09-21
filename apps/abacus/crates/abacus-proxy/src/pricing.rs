//! Per-provider/model µ$-per-token pricing → the priced `est_max`.
//!
//! Abacus must reserve the MAXIMUM a request could cost before it forwards it, so
//! the estimate is deliberately conservative: prompt tokens are priced at the
//! input rate, and the completion is priced at the FULL `max_tokens` ceiling (or
//! a provider default when the caller omits it). Every multiply-add rounds UP
//! ([`mul_div_ceil`]) so fractional-micro truncation can never under-reserve.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;

use abacus_core::error::AbacusError;
use abacus_core::estimate::CostEstimate;
use abacus_core::money::{mul_div_ceil, Micros};

/// A conservative per-part token floor for a non-text multimodal content part
/// (image / audio / file / unknown typed part). The true encoded token cost of a
/// base64 image or audio blob cannot be measured cheaply pre-flight, so we reserve
/// a generous fixed floor per part; the post-call capture trues this up to the
/// provider's reported usage, so over-reserving here only holds funds briefly.
pub const MULTIMODAL_PART_FLOOR_TOKENS: i64 = 1_600;

/// Per-token rates for one model, in µ$ per 1000 tokens (the common list-price
/// granularity), split by input vs output.
#[derive(Debug, Clone, Copy)]
pub struct ModelRate {
    /// Input/prompt price, µ$ per 1000 tokens.
    pub input_micros_per_1k: i64,
    /// Output/completion price, µ$ per 1000 tokens.
    pub output_micros_per_1k: i64,
}

/// When a request omits `max_tokens`, assume this completion ceiling so the
/// reserve still covers a worst-case generation.
pub const DEFAULT_MAX_COMPLETION_TOKENS: i64 = 4096;

/// One entry in the on-disk pricing config: a provider/model with its input and
/// output rates in µ$ per 1000 tokens.
#[derive(Debug, Clone, Deserialize)]
pub struct PricingConfigEntry {
    /// Provider key (matched against [`Config::provider`]).
    pub provider: String,
    /// Model id (matched against the request body's `model`).
    pub model: String,
    /// Input/prompt price, µ$ per 1000 tokens.
    pub input_micros_per_1k: i64,
    /// Output/completion price, µ$ per 1000 tokens.
    pub output_micros_per_1k: i64,
}

/// The on-disk pricing config document: a flat list of per-provider/model rates,
/// loaded from `ABACUS_PRICING_CONFIG`. Entries override the built-in table so any
/// provider/model an operator prices is honored instead of killed with
/// [`AbacusError::ModelNotPriced`]. The four rate fields are required, so a
/// mistyped rate key fails fast; other keys (e.g. a `_comment`) are ignored.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PricingConfig {
    /// The priced provider/model entries.
    #[serde(default)]
    pub models: Vec<PricingConfigEntry>,
}

/// A provider/model → [`ModelRate`] table.
#[derive(Debug, Clone, Default)]
pub struct PricingTable {
    rates: HashMap<(String, String), ModelRate>,
}

impl PricingTable {
    /// An empty table.
    pub fn new() -> Self {
        PricingTable {
            rates: HashMap::new(),
        }
    }

    /// A small built-in table for the common OpenAI-compatible models. Rates are
    /// conservative list prices in µ$ per 1000 tokens; production overrides this
    /// from config.
    pub fn builtin() -> Self {
        let mut t = PricingTable::new();
        t.insert(
            "openai",
            "gpt-4o",
            ModelRate {
                input_micros_per_1k: 2_500,
                output_micros_per_1k: 10_000,
            },
        );
        t.insert(
            "openai",
            "gpt-4o-mini",
            ModelRate {
                input_micros_per_1k: 150,
                output_micros_per_1k: 600,
            },
        );
        t.insert(
            "openai",
            "gpt-4.1",
            ModelRate {
                input_micros_per_1k: 2_000,
                output_micros_per_1k: 8_000,
            },
        );
        t
    }

    /// Build a table from [`builtin`](Self::builtin) overlaid with the entries in
    /// the JSON config file at `path` (the `ABACUS_PRICING_CONFIG` target). Config
    /// entries override built-ins for the same `(provider, model)`, so operators
    /// can price any provider/model — not just the three built-in OpenAI ones.
    ///
    /// Fails fast with [`AbacusError::Config`] on an unreadable file, malformed
    /// JSON, an empty provider/model, or a negative rate.
    pub fn from_config_file(path: &Path) -> Result<Self, AbacusError> {
        let raw = std::fs::read_to_string(path).map_err(|e| {
            AbacusError::Config(format!(
                "failed to read ABACUS_PRICING_CONFIG {}: {e}",
                path.display()
            ))
        })?;
        let config: PricingConfig = serde_json::from_str(&raw).map_err(|e| {
            AbacusError::Config(format!(
                "failed to parse ABACUS_PRICING_CONFIG {}: {e}",
                path.display()
            ))
        })?;
        Self::builtin().with_config(config)
    }

    /// Overlay parsed config entries on top of this table, returning the merged
    /// table. Every rate is validated non-negative and every key non-empty; an
    /// invalid entry is a fail-fast [`AbacusError::Config`].
    pub fn with_config(self, config: PricingConfig) -> Result<Self, AbacusError> {
        let mut table = self;
        for entry in config.models {
            if entry.provider.trim().is_empty() || entry.model.trim().is_empty() {
                return Err(AbacusError::Config(
                    "pricing entry provider and model must be non-empty".to_string(),
                ));
            }
            if entry.input_micros_per_1k < 0 || entry.output_micros_per_1k < 0 {
                return Err(AbacusError::Config(format!(
                    "pricing rate for {}/{} must be non-negative",
                    entry.provider, entry.model
                )));
            }
            table.insert(
                &entry.provider,
                &entry.model,
                ModelRate {
                    input_micros_per_1k: entry.input_micros_per_1k,
                    output_micros_per_1k: entry.output_micros_per_1k,
                },
            );
        }
        Ok(table)
    }

    /// Insert or override a rate for `(provider, model)`.
    pub fn insert(&mut self, provider: &str, model: &str, rate: ModelRate) {
        self.rates
            .insert((provider.to_string(), model.to_string()), rate);
    }

    /// Look up a rate; `None` when the model is not priced.
    pub fn rate(&self, provider: &str, model: &str) -> Option<ModelRate> {
        self.rates
            .get(&(provider.to_string(), model.to_string()))
            .copied()
    }

    /// Price the pre-flight estimate for a request `body` against `provider`.
    ///
    /// Prompt tokens are estimated from the serialized prompt text with a crude,
    /// deterministic ~4-chars-per-token heuristic (conservative rounding up); the
    /// completion is priced at the request's `max_tokens` or
    /// [`DEFAULT_MAX_COMPLETION_TOKENS`]. Errors with [`AbacusError::ModelNotPriced`]
    /// when the model has no rate.
    pub fn estimate(
        &self,
        provider: &str,
        body: &serde_json::Value,
    ) -> Result<CostEstimate, AbacusError> {
        let model = body
            .get("model")
            .and_then(|v| v.as_str())
            .ok_or(AbacusError::ModelNotPriced)?;
        let rate = self
            .rate(provider, model)
            .ok_or(AbacusError::ModelNotPriced)?;

        let prompt_tokens = estimate_prompt_tokens(body);
        let max_completion = body
            .get("max_tokens")
            .and_then(|v| v.as_i64())
            .filter(|&n| n > 0)
            .unwrap_or(DEFAULT_MAX_COMPLETION_TOKENS);

        let prompt_cost = mul_div_ceil(prompt_tokens, rate.input_micros_per_1k, 1000)?;
        let completion_cost = mul_div_ceil(max_completion, rate.output_micros_per_1k, 1000)?;
        let est_max = Micros::new(prompt_cost).checked_add(Micros::new(completion_cost))?;

        Ok(CostEstimate::new(
            Micros::new(prompt_cost),
            est_max,
            prompt_tokens,
            max_completion,
        ))
    }

    /// Price the EXACT actual cost from reported usage after the call completes.
    pub fn actual(
        &self,
        provider: &str,
        model: &str,
        usage: &abacus_core::transport::Usage,
    ) -> Result<Micros, AbacusError> {
        let rate = self
            .rate(provider, model)
            .ok_or(AbacusError::ModelNotPriced)?;
        let input = mul_div_ceil(usage.input_tokens.max(0), rate.input_micros_per_1k, 1000)?;
        let output = mul_div_ceil(usage.output_tokens.max(0), rate.output_micros_per_1k, 1000)?;
        Micros::new(input).checked_add(Micros::new(output))
    }
}

/// A deterministic, conservative prompt-token estimate.
///
/// Text is summed by character length and divided by 4, rounding UP. Beyond plain
/// string `messages[].content`, this also accounts for the payloads that the
/// naive string-only count silently dropped and under-reserved:
///
/// - **Structured (array) content** — the vision/multimodal shape where `content`
///   is an array of typed parts: every `text` part is counted, and every non-text
///   part (image / audio / file / unknown) reserves [`MULTIMODAL_PART_FLOOR_TOKENS`].
/// - **Tool-call payloads** — a message's `tool_calls` / `function_call` (function
///   name + JSON arguments) are billed as prompt on later turns.
/// - **Tool/function definitions** — top-level `tools` / `functions` the provider
///   injects into the prompt.
/// - **Completions `prompt`** — a string or an array of strings.
///
/// Serialized-JSON length over-counts vs. true tokenization and non-text parts use
/// a floor, so the estimate stays conservative (never under-reserves).
fn estimate_prompt_tokens(body: &serde_json::Value) -> i64 {
    let mut chars: i64 = 0;
    let mut multimodal_parts: i64 = 0;

    if let Some(messages) = body.get("messages").and_then(|v| v.as_array()) {
        for m in messages {
            match m.get("content") {
                Some(serde_json::Value::String(s)) => chars += s.chars().count() as i64,
                Some(serde_json::Value::Array(parts)) => {
                    for part in parts {
                        chars += text_chars_in_part(part, &mut multimodal_parts);
                    }
                }
                // An object content shape we do not recognize: reserve one floor.
                Some(other @ serde_json::Value::Object(_)) => {
                    chars += text_chars_in_part(other, &mut multimodal_parts);
                }
                _ => {}
            }
            // A tool/author `name` is prompt text too.
            if let Some(name) = m.get("name").and_then(|v| v.as_str()) {
                chars += name.chars().count() as i64;
            }
            // Assistant tool-call payloads (function name + JSON arguments) are
            // re-fed as prompt on the next turn; price their serialized length.
            if let Some(tool_calls) = m.get("tool_calls") {
                chars += serialized_chars(tool_calls);
            }
            if let Some(function_call) = m.get("function_call") {
                chars += serialized_chars(function_call);
            }
        }
    }

    // Tool / function DEFINITIONS are injected into the prompt by the provider.
    if let Some(tools) = body.get("tools") {
        chars += serialized_chars(tools);
    }
    if let Some(functions) = body.get("functions") {
        chars += serialized_chars(functions);
    }

    // Completions-style `prompt`: a string, or an array of strings.
    match body.get("prompt") {
        Some(serde_json::Value::String(s)) => chars += s.chars().count() as i64,
        Some(serde_json::Value::Array(items)) => {
            for item in items {
                if let Some(s) = item.as_str() {
                    chars += s.chars().count() as i64;
                }
            }
        }
        _ => {}
    }

    // ceil(chars / 4) text tokens, plus the conservative per-part multimodal floor.
    let text_tokens = if chars <= 0 { 0 } else { (chars + 3) / 4 };
    text_tokens.saturating_add(multimodal_parts.saturating_mul(MULTIMODAL_PART_FLOOR_TOKENS))
}

/// Count the `text` chars in one structured content part. A non-text part
/// (image / audio / file / typed-unknown) records a multimodal part (whose token
/// cost the caller floors) and contributes only any incidental `text` it carries.
fn text_chars_in_part(part: &serde_json::Value, multimodal_parts: &mut i64) -> i64 {
    if let Some(s) = part.as_str() {
        return s.chars().count() as i64;
    }
    let part_type = part.get("type").and_then(|v| v.as_str());
    match part_type {
        Some("text") | Some("input_text") => part
            .get("text")
            .and_then(|v| v.as_str())
            .map(|s| s.chars().count() as i64)
            .unwrap_or(0),
        _ => {
            // image_url / input_image / input_audio / file / unknown / untyped:
            // the true encoded cost is unmeasurable pre-flight → reserve a floor,
            // plus any incidental text (e.g. an alt string) the part carries.
            *multimodal_parts += 1;
            part.get("text")
                .and_then(|v| v.as_str())
                .map(|s| s.chars().count() as i64)
                .unwrap_or(0)
        }
    }
}

/// A conservative char count of an arbitrary JSON sub-tree, via its serialized
/// form. Used for tool-call / tool-definition payloads whose exact tokenization we
/// do not model pre-flight; serialized length over-counts, never under-counts.
fn serialized_chars(value: &serde_json::Value) -> i64 {
    serde_json::to_string(value)
        .map(|s| s.chars().count() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use abacus_core::transport::Usage;
    use serde_json::json;

    #[test]
    fn unpriced_model_errors() {
        let t = PricingTable::builtin();
        let body = json!({"model": "no-such-model", "messages": []});
        assert_eq!(
            t.estimate("openai", &body),
            Err(AbacusError::ModelNotPriced)
        );
    }

    #[test]
    fn est_max_prices_full_completion_ceiling() {
        let t = PricingTable::builtin();
        let body = json!({
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": "hello"}],
            "max_tokens": 1000
        });
        let est = t.estimate("openai", &body).unwrap();
        // completion: 1000 tok * 600 µ$/1k = 600 µ$; prompt: ceil(5/4)=2 tok *150/1k= ceil(0.3)=1.
        assert_eq!(est.max_completion_tokens, 1000);
        assert_eq!(est.est_max, Micros::new(601));
        assert_eq!(est.reserve_amount(), est.est_max);
    }

    #[test]
    fn missing_max_tokens_uses_default_ceiling() {
        let t = PricingTable::builtin();
        let body = json!({"model": "gpt-4o-mini", "messages": []});
        let est = t.estimate("openai", &body).unwrap();
        assert_eq!(est.max_completion_tokens, DEFAULT_MAX_COMPLETION_TOKENS);
    }

    #[test]
    fn config_prices_a_non_builtin_provider_model() {
        let config = PricingConfig {
            models: vec![PricingConfigEntry {
                provider: "anthropic".to_string(),
                model: "claude-3-5-sonnet".to_string(),
                input_micros_per_1k: 3_000,
                output_micros_per_1k: 15_000,
            }],
        };
        let t = PricingTable::builtin().with_config(config).unwrap();
        // Built-ins are still present after overlay.
        assert!(t.rate("openai", "gpt-4o").is_some());
        // The config model that would previously be ModelNotPriced now prices.
        let body = json!({
            "model": "claude-3-5-sonnet",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 1000
        });
        let est = t.estimate("anthropic", &body).unwrap();
        // completion: 1000 * 15_000/1k = 15_000 µ$; prompt: ceil(2/4)=1 tok * 3_000/1k = 3.
        assert_eq!(est.est_max, Micros::new(15_003));
    }

    #[test]
    fn config_overrides_a_builtin_rate() {
        let config = PricingConfig {
            models: vec![PricingConfigEntry {
                provider: "openai".to_string(),
                model: "gpt-4o-mini".to_string(),
                input_micros_per_1k: 1,
                output_micros_per_1k: 1,
            }],
        };
        let t = PricingTable::builtin().with_config(config).unwrap();
        let rate = t.rate("openai", "gpt-4o-mini").unwrap();
        assert_eq!(rate.output_micros_per_1k, 1);
    }

    #[test]
    fn config_rejects_negative_rate() {
        let config = PricingConfig {
            models: vec![PricingConfigEntry {
                provider: "openai".to_string(),
                model: "bad".to_string(),
                input_micros_per_1k: -1,
                output_micros_per_1k: 10,
            }],
        };
        assert!(matches!(
            PricingTable::builtin().with_config(config),
            Err(AbacusError::Config(_))
        ));
    }

    #[test]
    fn config_rejects_empty_key() {
        let config = PricingConfig {
            models: vec![PricingConfigEntry {
                provider: "".to_string(),
                model: "x".to_string(),
                input_micros_per_1k: 1,
                output_micros_per_1k: 1,
            }],
        };
        assert!(matches!(
            PricingTable::builtin().with_config(config),
            Err(AbacusError::Config(_))
        ));
    }

    #[test]
    fn array_content_sums_text_and_floors_image_parts() {
        // Two text parts (4 + 4 chars) plus one image part → floor reserved.
        let body = json!({
            "model": "gpt-4o",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "text", "text": "abcd"},
                    {"type": "text", "text": "efgh"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}
                ]
            }]
        });
        let tokens = estimate_prompt_tokens(&body);
        // text: ceil(8/4)=2 tokens; plus one multimodal floor.
        assert_eq!(tokens, 2 + MULTIMODAL_PART_FLOOR_TOKENS);
    }

    #[test]
    fn image_content_is_not_undercounted_to_zero() {
        // The OLD string-only estimator returned 0 for image-only content.
        let body = json!({
            "model": "gpt-4o",
            "messages": [{
                "role": "user",
                "content": [
                    {"type": "image_url", "image_url": {"url": "https://x/y.png"}}
                ]
            }]
        });
        assert_eq!(estimate_prompt_tokens(&body), MULTIMODAL_PART_FLOOR_TOKENS);
    }

    #[test]
    fn tool_calls_and_definitions_are_counted() {
        let with_tools = json!({
            "model": "gpt-4o",
            "messages": [{
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}
                }]
            }],
            "tools": [{
                "type": "function",
                "function": {"name": "get_weather", "description": "Get weather", "parameters": {}}
            }]
        });
        let bare = json!({
            "model": "gpt-4o",
            "messages": [{"role": "assistant", "content": null}]
        });
        // The tool payloads must add prompt tokens the bare body lacks.
        assert!(estimate_prompt_tokens(&with_tools) > estimate_prompt_tokens(&bare));
    }

    #[test]
    fn plain_string_content_still_counts_as_before() {
        // Regression guard: the string path is unchanged (5 chars → ceil(5/4)=2).
        let body = json!({
            "model": "gpt-4o-mini",
            "messages": [{"role": "user", "content": "hello"}]
        });
        assert_eq!(estimate_prompt_tokens(&body), 2);
    }

    #[test]
    fn actual_prices_reported_usage() {
        let t = PricingTable::builtin();
        let cost = t
            .actual(
                "openai",
                "gpt-4o-mini",
                &Usage {
                    input_tokens: 1000,
                    output_tokens: 1000,
                },
            )
            .unwrap();
        // 150 + 600 = 750 µ$.
        assert_eq!(cost, Micros::new(750));
    }
}
