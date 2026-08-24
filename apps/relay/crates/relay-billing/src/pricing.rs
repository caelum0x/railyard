//! Model-aware pricing — the per-model microdollar rate table.
//!
//! ADR-0001 invariant: money is integer microdollars end to end, so a model's
//! price is expressed as integer microdollars **per token** (prompt and
//! completion charged independently). A [`PriceTable`] maps a [`ModelId`] to its
//! [`ModelPrice`] and always carries a `default` price used as a SAFE FALLBACK
//! for any model the table does not know about — so an unpriced model is billed
//! deterministically rather than free or rejected.
//!
//! This replaces the flat, model-agnostic linear token rate the gateway used
//! before: the hold estimate and the final settled cost are both derived from
//! the requested model's actual rate. Pure data — no I/O, no float.

use std::collections::HashMap;

use crate::domain::ModelId;
use crate::money::Micros;

/// The per-token microdollar rate for a single model.
///
/// Prompt (input) and completion (output) tokens are priced independently, as
/// every real provider does. All arithmetic is saturating so a malformed token
/// count can never wrap the integer money path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelPrice {
    /// Microdollars charged per prompt/input token.
    pub prompt_micros_per_token: i64,
    /// Microdollars charged per completion/output token.
    pub completion_micros_per_token: i64,
}

impl ModelPrice {
    /// Construct a per-token price.
    pub const fn new(prompt_micros_per_token: i64, completion_micros_per_token: i64) -> Self {
        Self {
            prompt_micros_per_token,
            completion_micros_per_token,
        }
    }

    /// Exact integer-microdollar cost of `prompt` + `completion` tokens at this
    /// price. Saturating on the intermediate products and the sum keeps the
    /// computation total; the ledger still stores an exact integer.
    pub fn cost(&self, prompt_tokens: u32, completion_tokens: u32) -> Micros {
        let prompt_cost = (prompt_tokens as i64).saturating_mul(self.prompt_micros_per_token);
        let completion_cost =
            (completion_tokens as i64).saturating_mul(self.completion_micros_per_token);
        Micros::new(prompt_cost.saturating_add(completion_cost))
    }
}

/// A per-model microdollar price table with a mandatory safe-default fallback.
///
/// Lookups NEVER miss: an unknown model resolves to `default`. Build it once
/// (e.g. from provider config) and share it behind an `Arc`.
#[derive(Debug, Clone)]
pub struct PriceTable {
    prices: HashMap<ModelId, ModelPrice>,
    default: ModelPrice,
}

impl PriceTable {
    /// A table with no per-model overrides; every model resolves to `default`.
    pub fn new(default: ModelPrice) -> Self {
        Self {
            prices: HashMap::new(),
            default,
        }
    }

    /// Builder-style insert returning a new table (immutable-friendly wiring).
    pub fn with_model(mut self, model: ModelId, price: ModelPrice) -> Self {
        self.prices.insert(model, price);
        self
    }

    /// Insert or overwrite a single model's price in place.
    pub fn insert(&mut self, model: ModelId, price: ModelPrice) {
        self.prices.insert(model, price);
    }

    /// The safe-default price applied to unknown models.
    pub fn default_price(&self) -> ModelPrice {
        self.default
    }

    /// `true` when the table has an explicit price for `model` (i.e. the lookup
    /// will NOT fall back to the default).
    pub fn is_known(&self, model: &ModelId) -> bool {
        self.prices.contains_key(model)
    }

    /// The price for `model`, or the safe default when the model is unknown.
    pub fn price_for(&self, model: &ModelId) -> ModelPrice {
        self.prices.get(model).copied().unwrap_or(self.default)
    }

    /// Model-aware cost of `prompt` + `completion` tokens for `model`. Unknown
    /// models are billed at the safe default rate.
    pub fn cost(&self, model: &ModelId, prompt_tokens: u32, completion_tokens: u32) -> Micros {
        self.price_for(model).cost(prompt_tokens, completion_tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpt4o() -> ModelId {
        ModelId::namespace("openai/gpt-4o")
    }

    fn haiku() -> ModelId {
        ModelId::namespace("anthropic/claude-3.5-haiku")
    }

    #[test]
    fn model_price_costs_prompt_and_completion_independently() {
        let price = ModelPrice::new(3, 15);
        // 100 prompt * 3 + 50 completion * 15 = 300 + 750 = 1050
        assert_eq!(price.cost(100, 50), Micros::new(1_050));
    }

    #[test]
    fn model_price_cost_saturates_instead_of_wrapping() {
        let price = ModelPrice::new(i64::MAX, i64::MAX);
        // Saturating multiply/add keeps this total (never panics or wraps).
        assert_eq!(price.cost(u32::MAX, u32::MAX), Micros::new(i64::MAX));
    }

    #[test]
    fn cost_uses_the_per_model_price_from_a_fake_catalog() {
        let table = PriceTable::new(ModelPrice::new(10, 30))
            .with_model(gpt4o(), ModelPrice::new(3, 15))
            .with_model(haiku(), ModelPrice::new(1, 5));

        // gpt-4o: 1000*3 + 500*15 = 3000 + 7500 = 10_500
        assert_eq!(table.cost(&gpt4o(), 1000, 500), Micros::new(10_500));
        // haiku: 1000*1 + 500*5 = 1000 + 2500 = 3500
        assert_eq!(table.cost(&haiku(), 1000, 500), Micros::new(3_500));
    }

    #[test]
    fn unknown_model_falls_back_to_the_safe_default() {
        let table =
            PriceTable::new(ModelPrice::new(10, 30)).with_model(gpt4o(), ModelPrice::new(3, 15));
        let unknown = ModelId::namespace("who/knows");

        assert!(!table.is_known(&unknown));
        assert_eq!(table.price_for(&unknown), ModelPrice::new(10, 30));
        // default: 1000*10 + 500*30 = 10_000 + 15_000 = 25_000
        assert_eq!(table.cost(&unknown, 1000, 500), Micros::new(25_000));
    }

    #[test]
    fn known_model_is_reported_known_and_priced_at_its_rate() {
        let table =
            PriceTable::new(ModelPrice::new(10, 30)).with_model(gpt4o(), ModelPrice::new(3, 15));
        assert!(table.is_known(&gpt4o()));
        assert_eq!(table.price_for(&gpt4o()), ModelPrice::new(3, 15));
    }

    #[test]
    fn insert_overwrites_an_existing_model_price() {
        let mut table = PriceTable::new(ModelPrice::new(10, 30));
        table.insert(gpt4o(), ModelPrice::new(3, 15));
        table.insert(gpt4o(), ModelPrice::new(4, 16));
        assert_eq!(table.price_for(&gpt4o()), ModelPrice::new(4, 16));
    }

    #[test]
    fn default_price_is_reported() {
        let table = PriceTable::new(ModelPrice::new(7, 21));
        assert_eq!(table.default_price(), ModelPrice::new(7, 21));
    }

    #[test]
    fn zero_tokens_cost_nothing() {
        let table =
            PriceTable::new(ModelPrice::new(10, 30)).with_model(gpt4o(), ModelPrice::new(3, 15));
        assert_eq!(table.cost(&gpt4o(), 0, 0), Micros::ZERO);
    }
}
