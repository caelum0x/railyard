//! Per-provider/model µ$-per-token pricing → the priced `est_max`.
//!
//! Abacus must reserve the MAXIMUM a request could cost before it forwards it, so
//! the estimate is deliberately conservative: prompt tokens are priced at the
//! input rate, and the completion is priced at the FULL `max_tokens` ceiling (or
//! a provider default when the caller omits it). Every multiply-add rounds UP
//! ([`mul_div_ceil`]) so fractional-micro truncation can never under-reserve.

use std::collections::HashMap;

use abacus_core::error::AbacusError;
use abacus_core::estimate::CostEstimate;
use abacus_core::money::{mul_div_ceil, Micros};

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

/// A deterministic, conservative prompt-token estimate: sum the character length
/// of every `messages[].content` (plus a top-level `prompt`) and divide by 4,
/// rounding UP. This never under-counts short prompts to zero when text exists.
fn estimate_prompt_tokens(body: &serde_json::Value) -> i64 {
    let mut chars: i64 = 0;
    if let Some(messages) = body.get("messages").and_then(|v| v.as_array()) {
        for m in messages {
            if let Some(c) = m.get("content").and_then(|v| v.as_str()) {
                chars += c.chars().count() as i64;
            }
        }
    }
    if let Some(p) = body.get("prompt").and_then(|v| v.as_str()) {
        chars += p.chars().count() as i64;
    }
    // ceil(chars / 4); at least 1 token when any text is present.
    if chars == 0 {
        0
    } else {
        (chars + 3) / 4
    }
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
