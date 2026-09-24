//! The per-model price catalog — real provider list prices as a data table.
//!
//! This is the fix for the "one flat default rate for every model" mispricing:
//! instead of billing every model at [`crate::pricing::PriceTable`]'s safe
//! default, we seed the table with each major model's actual published list
//! price. Unknown models still fall back to the conservative default, so the
//! catalog only ever makes billing MORE accurate, never rejects a model.
//!
//! ## Units (ADR-0001: integer microdollars, no float on the money path)
//!
//! Providers publish prices as **USD per 1,000,000 tokens**. The billing engine
//! ([`ModelPrice`]) retains those exact integer **micro-USD per million token**
//! rates. Cost calculation multiplies tokens at full precision and rounds the
//! final request charge up to one microdollar, so `$0.10 / 1M` is never rounded
//! down to a free model. Everything is `i64`/`i128`; there is no float on the
//! money path.
//!
//! ## Adding a model
//!
//! Add one row to [`BUILTIN_PRICES`]: the raw (OpenRouter-style
//! `provider/model`) id and its input/output price in micro-USD per 1M tokens,
//! and the matching model to `relay-catalog/data/catalog.json` (a test fails
//! when the two drift apart).

use crate::domain::ModelId;
use crate::pricing::{ModelPrice, PriceTable};

/// Build a [`ModelPrice`] from input/output list prices quoted in micro-USD per
/// 1M tokens.
const fn price(
    prompt_micro_usd_per_million: i64,
    completion_micro_usd_per_million: i64,
) -> ModelPrice {
    ModelPrice::new(
        prompt_micro_usd_per_million,
        completion_micro_usd_per_million,
    )
}

/// The price catalog: `(raw_model_id, input_micro_usd_per_1M, output_micro_usd_per_1M)`.
///
/// `raw_model_id` is the un-namespaced OpenRouter-style `provider/model` id (the
/// id clients send); it is namespaced to `openrouter/{raw}` when inserted, which
/// is exactly the key the billing middleware looks up. Prices are published
/// list prices (USD per 1M tokens, expressed here in micro-USD). Where a
/// provider tiers by context length, the standard/base tier is used.
///
/// Snapshot of 2026-09-24: every row that OpenRouter still lists carries its
/// `GET https://openrouter.ai/api/v1/models` price (sub-micro fractions round
/// UP, never down). Rows marked "legacy list price" are models OpenRouter has
/// retired; they keep their last published price so existing callers are not
/// silently billed at the fallback. `:free` rows are the zero-priced
/// OpenRouter free tier. `groq/...` rows are Groq's published self-serve
/// prices (`tensorzero::model_name::groq::<model>` resolves to them); Groq
/// models without a published price are deliberately absent.
///
/// `relay-catalog/data/catalog.json` is the same table with capabilities and
/// context windows; a test keeps the two identical. At runtime the gateway can
/// refresh these prices from the live OpenRouter feed
/// (`RELAY_CATALOG_REFRESH_URL`, see `relay_catalog::openrouter`).
pub const BUILTIN_PRICES: &[(&str, i64, i64)] = &[
    // ---- anthropic ---------------------------------------------------
    ("anthropic/claude-3-haiku", 250_000, 1_250_000), // $0.25 / $1.25
    ("anthropic/claude-3-opus", 15_000_000, 75_000_000), // $15 / $75 (legacy list price)
    ("anthropic/claude-3.5-haiku", 800_000, 4_000_000), // $0.8 / $4 (legacy list price)
    ("anthropic/claude-3.5-sonnet", 3_000_000, 15_000_000), // $3 / $15 (legacy list price)
    ("anthropic/claude-3.7-sonnet", 3_000_000, 15_000_000), // $3 / $15 (legacy list price)
    ("anthropic/claude-fable-5", 10_000_000, 50_000_000), // $10 / $50
    ("anthropic/claude-fable-5.1", 10_000_000, 50_000_000), // $10 / $50
    ("anthropic/claude-haiku-4.5", 1_000_000, 5_000_000), // $1 / $5
    ("anthropic/claude-opus-4", 15_000_000, 75_000_000), // $15 / $75 (legacy list price)
    ("anthropic/claude-opus-4.1", 15_000_000, 75_000_000), // $15 / $75
    ("anthropic/claude-opus-4.7", 5_000_000, 25_000_000), // $5 / $25
    ("anthropic/claude-opus-4.8", 5_000_000, 25_000_000), // $5 / $25
    ("anthropic/claude-opus-5", 5_000_000, 25_000_000), // $5 / $25
    ("anthropic/claude-opus-5.5", 4_000_000, 20_000_000), // $4 / $20
    ("anthropic/claude-sonnet-4", 3_000_000, 15_000_000), // $3 / $15
    ("anthropic/claude-sonnet-4.5", 3_000_000, 15_000_000), // $3 / $15
    ("anthropic/claude-sonnet-4.6", 3_000_000, 15_000_000), // $3 / $15
    ("anthropic/claude-sonnet-5", 2_000_000, 10_000_000), // $2 / $10
    // ---- deepseek ----------------------------------------------------
    ("deepseek/deepseek-chat", 320_000, 890_000), // $0.32 / $0.89
    ("deepseek/deepseek-reasoner", 550_000, 2_190_000), // $0.55 / $2.19 (legacy list price)
    ("deepseek/deepseek-v4-flash", 88_606, 177_212), // $0.088606 / $0.177212
    ("deepseek/deepseek-v4-flash-0731", 30_000, 320_000), // $0.03 / $0.32
    ("deepseek/deepseek-v4-flash-vision-exp", 220_000, 660_000), // $0.22 / $0.66
    ("deepseek/deepseek-v4-pro", 939_600, 1_879_200), // $0.9396 / $1.8792
    ("deepseek/deepseek-v4-pro-0813", 462_000, 1_386_000), // $0.462 / $1.386
    ("deepseek/deepseek-v4.1-flash", 140_000, 420_000), // $0.14 / $0.42
    // ---- google ------------------------------------------------------
    ("google/gemini-1.5-flash", 75_000, 300_000), // $0.075 / $0.3 (legacy list price)
    ("google/gemini-1.5-pro", 1_250_000, 5_000_000), // $1.25 / $5 (legacy list price)
    ("google/gemini-2.0-flash", 100_000, 400_000), // $0.1 / $0.4 (legacy list price)
    ("google/gemini-2.5-flash", 300_000, 2_500_000), // $0.3 / $2.5
    ("google/gemini-2.5-pro", 1_250_000, 10_000_000), // $1.25 / $10
    ("google/gemini-3.1-flash-lite", 250_000, 1_500_000), // $0.25 / $1.5
    ("google/gemini-3.5-flash", 1_500_000, 9_000_000), // $1.5 / $9
    ("google/gemini-3.5-flash-lite", 300_000, 2_500_000), // $0.3 / $2.5
    ("google/gemini-3.6-flash", 750_000, 3_750_000), // $0.75 / $3.75
    ("google/gemini-3.7-flash", 750_000, 3_750_000), // $0.75 / $3.75
    ("google/gemini-3.8-flash", 750_000, 3_750_000), // $0.75 / $3.75
    ("google/gemma-4-26b-a4b-it", 90_000, 300_000), // $0.09 / $0.3
    ("google/gemma-4-26b-a4b-it:free", 0, 0), // $0 / $0
    ("google/gemma-4-31b-it", 90_000, 340_000), // $0.09 / $0.34
    ("google/gemma-4-31b-it:free", 0, 0), // $0 / $0
    // ---- groq --------------------------------------------------------
    ("groq/openai/gpt-oss-120b", 150_000, 600_000), // $0.15 / $0.6
    ("groq/openai/gpt-oss-20b", 75_000, 300_000), // $0.075 / $0.3
    // ---- meta-llama --------------------------------------------------
    ("meta-llama/llama-3.1-405b-instruct", 3_000_000, 3_000_000), // $3 / $3 (legacy list price)
    ("meta-llama/llama-3.1-70b-instruct", 400_000, 400_000), // $0.4 / $0.4
    ("meta-llama/llama-3.1-8b-instruct", 50_000, 80_000), // $0.05 / $0.08
    ("meta-llama/llama-3.3-70b-instruct", 100_000, 320_000), // $0.1 / $0.32
    ("meta-llama/llama-4-maverick", 187_500, 652_500), // $0.1875 / $0.6525
    ("meta-llama/llama-4-scout", 100_000, 300_000), // $0.1 / $0.3
    ("meta-llama/llama-guard-4-12b", 180_000, 180_000), // $0.18 / $0.18
    // ---- mistralai ---------------------------------------------------
    ("mistralai/ministral-14b-2512", 200_000, 200_000), // $0.2 / $0.2
    ("mistralai/ministral-3b-2512", 100_000, 100_000), // $0.1 / $0.1
    ("mistralai/ministral-8b-2512", 150_000, 150_000), // $0.15 / $0.15
    ("mistralai/mistral-large", 2_000_000, 6_000_000), // $2 / $6
    ("mistralai/mistral-medium-3-5", 1_500_000, 7_500_000), // $1.5 / $7.5
    ("mistralai/mistral-small", 200_000, 600_000), // $0.2 / $0.6 (legacy list price)
    ("mistralai/mistral-small-2603", 150_000, 600_000), // $0.15 / $0.6
    ("mistralai/mixtral-8x7b-instruct", 240_000, 240_000), // $0.24 / $0.24 (legacy list price)
    // ---- moonshotai --------------------------------------------------
    ("moonshotai/kimi-k2", 570_000, 2_300_000), // $0.57 / $2.3
    ("moonshotai/kimi-k2.5", 450_000, 2_250_000), // $0.45 / $2.25
    ("moonshotai/kimi-k2.6", 950_000, 4_000_000), // $0.95 / $4
    ("moonshotai/kimi-k2.7-code", 656_200, 3_300_000), // $0.6562 / $3.3
    ("moonshotai/kimi-k3", 3_000_000, 15_000_000), // $3 / $15
    // ---- nvidia ------------------------------------------------------
    ("nvidia/nemotron-3-super-120b-a12b:free", 0, 0), // $0 / $0
    ("nvidia/nemotron-3-ultra-550b-a55b:free", 0, 0), // $0 / $0
    // ---- openai ------------------------------------------------------
    ("openai/gpt-4-turbo", 10_000_000, 30_000_000), // $10 / $30
    ("openai/gpt-4.1", 2_000_000, 8_000_000), // $2 / $8
    ("openai/gpt-4.1-mini", 400_000, 1_600_000), // $0.4 / $1.6
    ("openai/gpt-4.1-nano", 100_000, 400_000), // $0.1 / $0.4
    ("openai/gpt-4o", 2_500_000, 10_000_000), // $2.5 / $10
    ("openai/gpt-4o-mini", 150_000, 600_000), // $0.15 / $0.6
    ("openai/gpt-5.6-luna", 200_000, 1_200_000), // $0.2 / $1.2
    ("openai/gpt-5.6-luna-pro", 200_000, 1_200_000), // $0.2 / $1.2
    ("openai/gpt-5.6-sol", 2_000_000, 10_000_000), // $2 / $10
    ("openai/gpt-5.6-sol-pro", 2_000_000, 10_000_000), // $2 / $10
    ("openai/gpt-5.6-terra", 2_000_000, 12_000_000), // $2 / $12
    ("openai/gpt-5.6-terra-pro", 2_000_000, 12_000_000), // $2 / $12
    ("openai/gpt-6-astra", 10_000_000, 50_000_000), // $10 / $50
    ("openai/gpt-6-astra-pro", 10_000_000, 50_000_000), // $10 / $50
    ("openai/gpt-6-luna", 100_000, 500_000), // $0.1 / $0.5
    ("openai/gpt-6-luna-pro", 100_000, 500_000), // $0.1 / $0.5
    ("openai/gpt-6-sol", 2_000_000, 10_000_000), // $2 / $10
    ("openai/gpt-6-sol-pro", 2_000_000, 10_000_000), // $2 / $10
    ("openai/gpt-oss-120b", 150_000, 600_000), // $0.15 / $0.6
    ("openai/gpt-oss-20b", 18_000, 90_000), // $0.018 / $0.09
    ("openai/o1", 15_000_000, 60_000_000), // $15 / $60
    ("openai/o1-mini", 1_100_000, 4_400_000), // $1.1 / $4.4 (legacy list price)
    ("openai/o3", 2_000_000, 8_000_000), // $2 / $8
    ("openai/o3-mini", 1_100_000, 4_400_000), // $1.1 / $4.4
    ("openai/o4-mini", 1_100_000, 4_400_000), // $1.1 / $4.4
    // ---- qwen --------------------------------------------------------
    ("qwen/qwen3-32b", 80_000, 280_000), // $0.08 / $0.28
    ("qwen/qwen3.7-flash", 30_000, 130_000), // $0.03 / $0.13
    ("qwen/qwen3.7-plus", 320_000, 1_280_000), // $0.32 / $1.28
    ("qwen/qwen3.8-2.4t-a95b", 2_000_000, 6_000_000), // $2 / $6
    ("qwen/qwen3.8-27b", 420_000, 3_000_000), // $0.42 / $3
    ("qwen/qwen3.8-27b:free", 0, 0), // $0 / $0
    ("qwen/qwen3.8-flash", 150_000, 470_000), // $0.15 / $0.47
    ("qwen/qwen3.8-max-0902", 2_000_000, 6_000_000), // $2 / $6
    ("qwen/qwen3.8-max-prime", 4_000_000, 12_000_000), // $4 / $12
    ("qwen/qwen3.8-omni-flash", 150_000, 470_000), // $0.15 / $0.47
    // ---- x-ai --------------------------------------------------------
    ("x-ai/grok-4.3", 1_250_000, 2_500_000), // $1.25 / $2.5
    ("x-ai/grok-4.5", 2_000_000, 6_000_000), // $2 / $6
    ("x-ai/grok-4.6", 2_000_000, 6_000_000), // $2 / $6
    ("x-ai/grok-4.7", 1_600_000, 4_800_000), // $1.6 / $4.8
    ("x-ai/grok-build-0.1", 1_000_000, 2_000_000), // $1 / $2
    // ---- z-ai --------------------------------------------------------
    ("z-ai/glm-5.2", 649_600, 2_041_600), // $0.6496 / $2.0416
    ("z-ai/glm-5.2:free", 0, 0), // $0 / $0
    ("z-ai/glm-5.3", 840_000, 2_640_000), // $0.84 / $2.64
    ("z-ai/glm-5.3-flash", 150_000, 500_000), // $0.15 / $0.5
    ("z-ai/glm-5.3-flashx", 370_000, 1_250_000), // $0.37 / $1.25
    ("z-ai/glm-5.3-prime", 2_800_000, 8_800_000), // $2.8 / $8.8
];

/// Build a [`PriceTable`] seeded with the full per-model [`CATALOG`], using
/// `default` as the conservative fallback for any model not in the catalog.
///
/// This is the single entry point the gateway wires into billing: known models
/// resolve to their accurate per-million rate, unknown models resolve to `default`.
pub fn price_table(default: ModelPrice) -> PriceTable {
    let mut table = PriceTable::new(default);
    for (raw_id, prompt_per_million, completion_per_million) in BUILTIN_PRICES {
        table.insert(
            ModelId::namespace(raw_id),
            price(*prompt_per_million, *completion_per_million),
        );
    }
    table
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deliberately smaller test fallback. Production's gateway default is
    /// `$15/$75` per 1M; catalog tests only verify fallback mechanics here.
    fn default_price() -> ModelPrice {
        ModelPrice::new(10_000_000, 30_000_000)
    }

    fn model(raw: &str) -> ModelId {
        ModelId::namespace(raw)
    }

    #[test]
    fn known_frontier_models_resolve_to_their_exact_list_rate() {
        let table = price_table(default_price());
        // Anthropic
        assert_eq!(
            table.price_for(&model("anthropic/claude-3-opus")),
            ModelPrice::new(15_000_000, 75_000_000)
        );
        assert_eq!(
            table.price_for(&model("anthropic/claude-3.5-sonnet")),
            ModelPrice::new(3_000_000, 15_000_000)
        );
        assert_eq!(
            table.price_for(&model("anthropic/claude-3.5-haiku")),
            ModelPrice::new(800_000, 4_000_000)
        );
        // OpenAI
        assert_eq!(
            table.price_for(&model("openai/gpt-4o")),
            ModelPrice::new(2_500_000, 10_000_000)
        );
        assert_eq!(
            table.price_for(&model("openai/gpt-4-turbo")),
            ModelPrice::new(10_000_000, 30_000_000)
        );
        assert_eq!(
            table.price_for(&model("openai/o1")),
            ModelPrice::new(15_000_000, 60_000_000)
        );
        // Google
        assert_eq!(
            table.price_for(&model("google/gemini-2.5-pro")),
            ModelPrice::new(1_250_000, 10_000_000)
        );
        // Mistral
        assert_eq!(
            table.price_for(&model("mistralai/mistral-large")),
            ModelPrice::new(2_000_000, 6_000_000)
        );
    }

    #[test]
    fn known_model_is_reported_known() {
        let table = price_table(default_price());
        assert!(table.is_known(&model("openai/gpt-4o")));
        assert!(table.is_known(&model("anthropic/claude-3-opus")));
    }

    #[test]
    fn unknown_model_falls_back_to_the_conservative_default() {
        let table = price_table(default_price());
        let unknown = model("some-startup/frontier-42b");
        assert!(!table.is_known(&unknown));
        assert_eq!(table.price_for(&unknown), default_price());
        // default: 1000*$10/1M + 500*$30/1M = 25,000 micro-USD.
        assert_eq!(
            table.cost(&unknown, 1000, 500),
            crate::money::Micros::new(25_000)
        );
    }

    #[test]
    fn cost_is_model_aware_for_a_known_model() {
        let table = price_table(default_price());
        // gpt-4o = ($2.50/$10 per 1M): 1000 + 500 tokens = 7,500 micro-USD.
        assert_eq!(
            table.cost(&model("openai/gpt-4o"), 1000, 500),
            crate::money::Micros::new(7_500)
        );
    }

    #[test]
    fn every_catalog_id_is_namespaced_and_resolvable() {
        let table = price_table(default_price());
        for (raw_id, _, _) in BUILTIN_PRICES {
            let id = ModelId::namespace(raw_id);
            assert!(
                id.as_str().starts_with("openrouter/"),
                "catalog key {raw_id} must namespace under openrouter/"
            );
            assert!(
                table.is_known(&id),
                "catalog model {raw_id} must resolve as known"
            );
        }
    }

    #[test]
    fn current_frontier_free_tier_and_groq_rows_are_priced() {
        let table = price_table(default_price());
        assert_eq!(
            table.price_for(&model("anthropic/claude-opus-5.5")),
            ModelPrice::new(4_000_000, 20_000_000)
        );
        assert_eq!(
            table.price_for(&model("openai/gpt-6-sol")),
            ModelPrice::new(2_000_000, 10_000_000)
        );
        // The OpenRouter free tier is genuinely free, not the fallback rate.
        assert!(table.is_known(&model("google/gemma-4-31b-it:free")));
        assert_eq!(
            table.price_for(&model("google/gemma-4-31b-it:free")),
            ModelPrice::new(0, 0)
        );
        // Groq-served routes resolve from their TensorZero selector.
        assert_eq!(
            table.price_for(&ModelId::from_tensorzero_selector(
                "tensorzero::model_name::groq::openai/gpt-oss-120b"
            )),
            ModelPrice::new(150_000, 600_000)
        );
    }

    #[test]
    fn catalog_has_no_duplicate_ids() {
        let mut seen = std::collections::HashSet::new();
        for (raw_id, _, _) in BUILTIN_PRICES {
            assert!(seen.insert(*raw_id), "duplicate catalog id: {raw_id}");
        }
    }
}
