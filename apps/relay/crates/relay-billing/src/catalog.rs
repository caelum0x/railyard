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
//! ([`ModelPrice`]) charges in **integer micro-USD per token**. The exact
//! relationship is:
//!
//! ```text
//! micro_usd_per_token = usd_per_1M_tokens        // numerically equal
//!                     = micro_usd_per_1M / 1e6
//! ```
//!
//! so a model listed at `$3.00 / 1M` costs `3` micro-USD per token, and one at
//! `$15.00 / 1M` costs `15`. Each [`CATALOG`] entry therefore stores the list
//! price as **micro-USD per 1M tokens** (an integer = `usd * 1_000_000`), which
//! [`per_token_micros`] divides down to the engine's per-token unit with
//! half-up rounding — the same rounding convention as
//! [`crate::money::usd_to_micros`]. Everything is `i64`; there is no float on
//! the money path.
//!
//! ## Granularity note (documented limitation)
//!
//! One micro-USD per token is `$1.00 / 1M` — the finest rate this unit can
//! express. Frontier models ($3–$75 / 1M) map exactly, but ultra-cheap models
//! (e.g. `$0.10 / 1M` input) round toward `0` at this granularity. Rounding to
//! the nearest integer is still far closer to the truth than the flat default,
//! and undercharging a fraction of a micro on a `$0.10 / 1M` model is
//! economically negligible. If sub-`$1/1M` fidelity is ever required, that is a
//! follow-up ADR to move the engine to a per-million-token rate; it is
//! deliberately out of scope for this additive catalog.
//!
//! ## Adding a model
//!
//! Add one row to [`CATALOG`]: the raw (OpenRouter-style `provider/model`) id
//! and its input/output price in micro-USD per 1M tokens. That is the only
//! change required — the table is data-driven.

use crate::domain::ModelId;
use crate::pricing::{ModelPrice, PriceTable};

/// One whole dollar of list price expressed in micro-USD per 1M tokens, and
/// equivalently the token count a per-1M price is quoted against (both `1e6`).
/// Multiplying makes the [`CATALOG`] rows read as their dollar prices
/// (`3 * USD_PER_MILLION` is `$3.00 / 1M`); dividing a micro-USD-per-1M price by
/// it yields micro-USD per single token.
const USD_PER_MILLION: i64 = 1_000_000;

/// Convert a list price quoted in **micro-USD per 1,000,000 tokens** into the
/// engine's **micro-USD per token**, rounding half-up (matching
/// [`crate::money::usd_to_micros`]). `const` so the whole catalog is built from
/// compile-time constants with no float anywhere.
const fn per_token_micros(micro_usd_per_million: i64) -> i64 {
    // Half-up rounding for a non-negative integer: (x + 0.5*scale) / scale.
    (micro_usd_per_million + USD_PER_MILLION / 2) / USD_PER_MILLION
}

/// Build a [`ModelPrice`] from input/output list prices quoted in micro-USD per
/// 1M tokens.
const fn price(
    prompt_micro_usd_per_million: i64,
    completion_micro_usd_per_million: i64,
) -> ModelPrice {
    ModelPrice::new(
        per_token_micros(prompt_micro_usd_per_million),
        per_token_micros(completion_micro_usd_per_million),
    )
}

/// The price catalog: `(raw_model_id, input_micro_usd_per_1M, output_micro_usd_per_1M)`.
///
/// `raw_model_id` is the un-namespaced OpenRouter-style `provider/model` id (the
/// id clients send); it is namespaced to `openrouter/{raw}` when inserted, which
/// is exactly the key the billing middleware looks up. Prices are published
/// provider list prices (USD per 1M tokens, expressed here in micro-USD). Where a
/// provider tiers by context length, the standard/base tier is used.
const CATALOG: &[(&str, i64, i64)] = &[
    // ---- Anthropic (claude opus / sonnet / haiku families) --------------
    (
        "anthropic/claude-3-opus",
        15 * USD_PER_MILLION,
        75 * USD_PER_MILLION,
    ), // $15 / $75
    (
        "anthropic/claude-opus-4",
        15 * USD_PER_MILLION,
        75 * USD_PER_MILLION,
    ), // $15 / $75
    (
        "anthropic/claude-opus-4.1",
        15 * USD_PER_MILLION,
        75 * USD_PER_MILLION,
    ), // $15 / $75
    (
        "anthropic/claude-3.5-sonnet",
        3 * USD_PER_MILLION,
        15 * USD_PER_MILLION,
    ), // $3 / $15
    (
        "anthropic/claude-3.7-sonnet",
        3 * USD_PER_MILLION,
        15 * USD_PER_MILLION,
    ), // $3 / $15
    (
        "anthropic/claude-sonnet-4",
        3 * USD_PER_MILLION,
        15 * USD_PER_MILLION,
    ), // $3 / $15
    (
        "anthropic/claude-sonnet-4.5",
        3 * USD_PER_MILLION,
        15 * USD_PER_MILLION,
    ), // $3 / $15
    ("anthropic/claude-3.5-haiku", 800_000, 4 * USD_PER_MILLION), // $0.80 / $4
    (
        "anthropic/claude-haiku-4.5",
        USD_PER_MILLION,
        5 * USD_PER_MILLION,
    ), // $1 / $5
    ("anthropic/claude-3-haiku", 250_000, 1_250_000),             // $0.25 / $1.25
    // ---- OpenAI (gpt-4o / 4o-mini / gpt-4.1 / o-series) -----------------
    ("openai/gpt-4o", 2_500_000, 10 * USD_PER_MILLION), // $2.50 / $10
    ("openai/gpt-4o-mini", 150_000, 600_000),           // $0.15 / $0.60
    (
        "openai/gpt-4-turbo",
        10 * USD_PER_MILLION,
        30 * USD_PER_MILLION,
    ), // $10 / $30
    ("openai/gpt-4.1", 2 * USD_PER_MILLION, 8 * USD_PER_MILLION), // $2 / $8
    ("openai/gpt-4.1-mini", 400_000, 1_600_000),        // $0.40 / $1.60
    ("openai/gpt-4.1-nano", 100_000, 400_000),          // $0.10 / $0.40
    ("openai/o1", 15 * USD_PER_MILLION, 60 * USD_PER_MILLION), // $15 / $60
    ("openai/o1-mini", 1_100_000, 4_400_000),           // $1.10 / $4.40
    ("openai/o3", 2 * USD_PER_MILLION, 8 * USD_PER_MILLION), // $2 / $8
    ("openai/o3-mini", 1_100_000, 4_400_000),           // $1.10 / $4.40
    ("openai/o4-mini", 1_100_000, 4_400_000),           // $1.10 / $4.40
    // ---- Google (gemini 2.x flash / pro, 1.5 family) --------------------
    ("google/gemini-2.5-pro", 1_250_000, 10 * USD_PER_MILLION), // $1.25 / $10
    ("google/gemini-2.5-flash", 300_000, 2_500_000),            // $0.30 / $2.50
    ("google/gemini-2.0-flash", 100_000, 400_000),              // $0.10 / $0.40
    ("google/gemini-1.5-pro", 1_250_000, 5 * USD_PER_MILLION),  // $1.25 / $5
    ("google/gemini-1.5-flash", 75_000, 300_000),               // $0.075 / $0.30
    // ---- DeepSeek -------------------------------------------------------
    ("deepseek/deepseek-chat", 270_000, 1_100_000), // $0.27 / $1.10
    ("deepseek/deepseek-reasoner", 550_000, 2_190_000), // $0.55 / $2.19
    // ---- Mistral --------------------------------------------------------
    (
        "mistralai/mistral-large",
        2 * USD_PER_MILLION,
        6 * USD_PER_MILLION,
    ), // $2 / $6
    ("mistralai/mistral-small", 200_000, 600_000), // $0.20 / $0.60
    ("mistralai/mixtral-8x7b-instruct", 240_000, 240_000), // $0.24 / $0.24
    // ---- Meta Llama (served via groq / together) ------------------------
    ("meta-llama/llama-3.3-70b-instruct", 590_000, 790_000), // $0.59 / $0.79
    ("meta-llama/llama-3.1-70b-instruct", 590_000, 790_000), // $0.59 / $0.79
    ("meta-llama/llama-3.1-8b-instruct", 50_000, 80_000),    // $0.05 / $0.08
    (
        "meta-llama/llama-3.1-405b-instruct",
        3 * USD_PER_MILLION,
        3 * USD_PER_MILLION,
    ), // $3 / $3
];

/// Build a [`PriceTable`] seeded with the full per-model [`CATALOG`], using
/// `default` as the conservative fallback for any model not in the catalog.
///
/// This is the single entry point the gateway wires into billing: known models
/// resolve to their accurate per-token rate, unknown models resolve to `default`.
pub fn price_table(default: ModelPrice) -> PriceTable {
    let mut table = PriceTable::new(default);
    for (raw_id, prompt_per_million, completion_per_million) in CATALOG {
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

    /// The conservative default used in tests (mirrors the gateway's
    /// `default_model_price()` = $10/$30 per 1M).
    fn default_price() -> ModelPrice {
        ModelPrice::new(10, 30)
    }

    fn model(raw: &str) -> ModelId {
        ModelId::namespace(raw)
    }

    #[test]
    fn per_token_micros_rounds_half_up() {
        assert_eq!(per_token_micros(3 * USD_PER_MILLION), 3); // $3/1M -> 3
        assert_eq!(per_token_micros(2_500_000), 3); // $2.50/1M -> 2.5 -> 3 (half up)
        assert_eq!(per_token_micros(600_000), 1); // $0.60/1M -> 0.6 -> 1
        assert_eq!(per_token_micros(400_000), 0); // $0.40/1M -> 0.4 -> 0
        assert_eq!(per_token_micros(1_500_000), 2); // $1.50/1M -> 1.5 -> 2
        assert_eq!(per_token_micros(0), 0);
    }

    #[test]
    fn known_frontier_models_resolve_to_their_exact_list_rate() {
        let table = price_table(default_price());
        // Anthropic
        assert_eq!(
            table.price_for(&model("anthropic/claude-3-opus")),
            ModelPrice::new(15, 75)
        );
        assert_eq!(
            table.price_for(&model("anthropic/claude-3.5-sonnet")),
            ModelPrice::new(3, 15)
        );
        assert_eq!(
            table.price_for(&model("anthropic/claude-3.5-haiku")),
            ModelPrice::new(1, 4) // $0.80 -> 1, $4 -> 4
        );
        // OpenAI
        assert_eq!(
            table.price_for(&model("openai/gpt-4o")),
            ModelPrice::new(3, 10) // $2.50 -> 3, $10 -> 10
        );
        assert_eq!(
            table.price_for(&model("openai/gpt-4-turbo")),
            ModelPrice::new(10, 30)
        );
        assert_eq!(
            table.price_for(&model("openai/o1")),
            ModelPrice::new(15, 60)
        );
        // Google
        assert_eq!(
            table.price_for(&model("google/gemini-2.5-pro")),
            ModelPrice::new(1, 10)
        );
        // Mistral
        assert_eq!(
            table.price_for(&model("mistralai/mistral-large")),
            ModelPrice::new(2, 6)
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
        // default: 1000*10 + 500*30 = 10_000 + 15_000 = 25_000
        assert_eq!(
            table.cost(&unknown, 1000, 500),
            crate::money::Micros::new(25_000)
        );
    }

    #[test]
    fn cost_is_model_aware_for_a_known_model() {
        let table = price_table(default_price());
        // gpt-4o = (3, 10): 1000*3 + 500*10 = 3000 + 5000 = 8000
        assert_eq!(
            table.cost(&model("openai/gpt-4o"), 1000, 500),
            crate::money::Micros::new(8_000)
        );
    }

    #[test]
    fn every_catalog_id_is_namespaced_and_resolvable() {
        let table = price_table(default_price());
        for (raw_id, _, _) in CATALOG {
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
    fn catalog_has_no_duplicate_ids() {
        let mut seen = std::collections::HashSet::new();
        for (raw_id, _, _) in CATALOG {
            assert!(seen.insert(*raw_id), "duplicate catalog id: {raw_id}");
        }
    }
}
