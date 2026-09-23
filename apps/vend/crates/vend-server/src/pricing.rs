//! Model-aware, integer-only rate cards for bounded holds and usage rating.
//!
//! Vend deliberately has no fallback price. Every allowed model must have an
//! operator-supplied rate card, otherwise the request fails before Mint is
//! touched. The hold covers the configured maximum input plus the request's
//! output ceiling. Terminal settlement uses Relay's authoritative
//! `tensorzero_cost`, not this operator estimate.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use vend_core::error::VendError;
use vend_core::money::Micros;

const TOKENS_PER_MILLION: i128 = 1_000_000;

/// One model's operator-owned billing rate and enforceable usage bounds.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ModelRate {
    /// Input price in microdollars per million tokens.
    pub input_micros_per_million_tokens: i64,
    /// Output price in microdollars per million tokens.
    pub output_micros_per_million_tokens: i64,
    /// Maximum reported input tokens Vend will accept for settlement.
    pub max_input_tokens: i64,
    /// Maximum request/output token ceiling.
    pub max_output_tokens: i64,
}

impl ModelRate {
    fn validate(self) -> Result<Self, VendError> {
        if self.input_micros_per_million_tokens < 0
            || self.output_micros_per_million_tokens < 0
            || self.max_input_tokens <= 0
            || self.max_output_tokens <= 0
        {
            return Err(VendError::Config("invalid model rate card".to_string()));
        }
        Ok(self)
    }
}

/// Immutable model-id to rate mapping loaded at process startup.
#[derive(Debug, Clone)]
pub struct PricingCatalog {
    rates: HashMap<String, ModelRate>,
}

impl PricingCatalog {
    /// Parse and validate `VEND_MODEL_RATES_JSON`.
    pub fn from_json(raw: &str) -> Result<Self, VendError> {
        let rates: HashMap<String, ModelRate> = serde_json::from_str(raw)
            .map_err(|error| VendError::Config(format!("VEND_MODEL_RATES_JSON: {error}")))?;
        if rates.is_empty() {
            return Err(VendError::Config(
                "VEND_MODEL_RATES_JSON must contain at least one model".to_string(),
            ));
        }
        for (model, rate) in &rates {
            if model.trim().is_empty() {
                return Err(VendError::Config("model id cannot be empty".to_string()));
            }
            rate.validate()?;
        }
        Ok(Self { rates })
    }

    /// Build a catalog in tests without environment mutation.
    pub fn new(rates: impl IntoIterator<Item = (String, ModelRate)>) -> Result<Self, VendError> {
        let rates: HashMap<_, _> = rates.into_iter().collect();
        let raw =
            serde_json::to_string(&rates).map_err(|error| VendError::Config(error.to_string()))?;
        Self::from_json(&raw)
    }

    /// Every priced model, sorted by id (backs `GET /v1/models`).
    pub fn models(&self) -> Vec<(String, ModelRate)> {
        let mut out: Vec<(String, ModelRate)> = self
            .rates
            .iter()
            .map(|(model, rate)| (model.clone(), *rate))
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn rate(&self, model: &str) -> Result<ModelRate, VendError> {
        self.rates
            .get(model)
            .copied()
            .ok_or_else(|| VendError::Config(format!("no rate card for model {model}")))
    }

    /// Conservative provider-cost ceiling used for the Mint hold.
    pub fn estimate_provider_cost(
        &self,
        model: &str,
        body: &serde_json::Value,
    ) -> Result<Micros, VendError> {
        let rate = self.rate(model)?;
        let output_ceiling = body
            .get("max_completion_tokens")
            .or_else(|| body.get("max_tokens"))
            .map(|value| value.as_i64().ok_or(VendError::InvalidAmount))
            .transpose()?
            .unwrap_or(rate.max_output_tokens);
        if output_ceiling <= 0 || output_ceiling > rate.max_output_tokens {
            return Err(VendError::InvalidAmount);
        }
        rated_cost(rate.max_input_tokens, output_ceiling, rate)
    }
}

fn rated_cost(input_tokens: i64, output_tokens: i64, rate: ModelRate) -> Result<Micros, VendError> {
    fn component(tokens: i64, micros_per_million: i64) -> Result<i128, VendError> {
        let numerator = (tokens as i128)
            .checked_mul(micros_per_million as i128)
            .ok_or(VendError::Overflow)?;
        Ok((numerator + TOKENS_PER_MILLION - 1) / TOKENS_PER_MILLION)
    }

    let total = component(input_tokens, rate.input_micros_per_million_tokens)?
        .checked_add(component(
            output_tokens,
            rate.output_micros_per_million_tokens,
        )?)
        .ok_or(VendError::Overflow)?;
    Ok(Micros::new(
        i64::try_from(total).map_err(|_| VendError::Overflow)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn catalog() -> PricingCatalog {
        PricingCatalog::new([(
            "model-a".to_string(),
            ModelRate {
                input_micros_per_million_tokens: 2_000_000,
                output_micros_per_million_tokens: 8_000_000,
                max_input_tokens: 100,
                max_output_tokens: 20,
            },
        )])
        .unwrap()
    }

    #[test]
    fn estimate_covers_max_input_and_requested_output() {
        assert_eq!(
            catalog()
                .estimate_provider_cost("model-a", &json!({"max_tokens": 10}))
                .unwrap()
                .get(),
            280
        );
    }

    #[test]
    fn unknown_model_fails_closed() {
        assert!(catalog()
            .estimate_provider_cost("missing", &json!({}))
            .is_err());
    }
}
