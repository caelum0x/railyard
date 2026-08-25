//! Operator-supplied, file-driven price-catalog overrides.
//!
//! The built-in [`crate::catalog::CATALOG`] ships a compile-time table of real
//! provider list prices, but an operator running relay against a private or
//! newly-launched model — or one whose contracted rate differs from the public
//! list price — needs to adjust pricing WITHOUT recompiling the gateway. This
//! module is that seam: a small, validated JSON config that is OVERLAID on the
//! embedded catalog at startup.
//!
//! ## Semantics (additive & backward-compatible)
//!
//! * The embedded catalog is always the base. A config file only ever
//!   ADDS models the catalog lacks or OVERRIDES ones it already has.
//! * An optional `default` block overrides the conservative safe-default price
//!   applied to any model neither the catalog nor the config knows about.
//! * If no config path is wired, behavior is identical to the embedded catalog —
//!   nothing changes for existing deployments.
//!
//! ## Units (ADR-0001: integer microdollars, no float on the money path)
//!
//! Prices are quoted exactly as the embedded [`crate::catalog`] quotes them:
//! **integer micro-USD per 1,000,000 tokens** (i.e. `usd_per_1M * 1_000_000`).
//! They are converted to the engine's per-token unit with the same half-up
//! rounding via [`crate::catalog::per_token_micros`]. There is no float anywhere.
//!
//! ## Format (JSON)
//!
//! ```json
//! {
//!   "default": {
//!     "input_micro_usd_per_million": 12000000,
//!     "output_micro_usd_per_million": 36000000
//!   },
//!   "models": [
//!     {
//!       "model": "openai/gpt-5",
//!       "input_micro_usd_per_million": 5000000,
//!       "output_micro_usd_per_million": 15000000
//!     }
//!   ]
//! }
//! ```
//!
//! The loader is strict: unknown fields, negative prices, empty model ids, and
//! duplicate model ids are all hard errors, so a malformed catalog is rejected
//! at startup rather than silently mispricing a money path.

use std::collections::HashSet;
use std::path::Path;

use serde::Deserialize;
use thiserror::Error;

use crate::catalog::{per_token_micros, price_table};
use crate::domain::ModelId;
use crate::pricing::{ModelPrice, PriceTable};

/// A failure loading, parsing, or validating an external price catalog.
///
/// `Clone`/`Eq` so callers (and tests) can compare and propagate it as ordinary
/// data; the underlying I/O and parse causes are captured as their rendered
/// message rather than as non-`Clone` source errors.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CatalogConfigError {
    /// The catalog file could not be read from disk.
    #[error("failed to read price catalog file '{path}': {message}")]
    Io { path: String, message: String },

    /// The catalog file was not valid JSON, or had an unexpected shape/field.
    #[error("failed to parse price catalog: {0}")]
    Parse(String),

    /// A price was negative; every rate must be `>= 0` micro-USD.
    #[error("model '{model}' has a negative {field} price ({value}); prices must be >= 0")]
    NegativePrice {
        model: String,
        field: &'static str,
        value: i64,
    },

    /// A model row carried an empty (or whitespace-only) model id.
    #[error("price catalog contains an entry with an empty model id")]
    EmptyModelId,

    /// The same model id appeared more than once, which would make the effective
    /// price order-dependent — rejected so the catalog is unambiguous.
    #[error("price catalog contains duplicate model id '{0}'")]
    DuplicateModel(String),
}

/// A single input/output rate, quoted in micro-USD per 1M tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceRow {
    /// Prompt/input list price, micro-USD per 1,000,000 tokens.
    pub input_micro_usd_per_million: i64,
    /// Completion/output list price, micro-USD per 1,000,000 tokens.
    pub output_micro_usd_per_million: i64,
}

impl PriceRow {
    /// Convert this per-1M list price into the engine's per-token [`ModelPrice`].
    fn to_model_price(self) -> ModelPrice {
        ModelPrice::new(
            per_token_micros(self.input_micro_usd_per_million),
            per_token_micros(self.output_micro_usd_per_million),
        )
    }

    /// Reject a negative rate on either leg.
    fn validate(self, model: &str) -> Result<(), CatalogConfigError> {
        if self.input_micro_usd_per_million < 0 {
            return Err(CatalogConfigError::NegativePrice {
                model: model.to_string(),
                field: "input",
                value: self.input_micro_usd_per_million,
            });
        }
        if self.output_micro_usd_per_million < 0 {
            return Err(CatalogConfigError::NegativePrice {
                model: model.to_string(),
                field: "output",
                value: self.output_micro_usd_per_million,
            });
        }
        Ok(())
    }
}

/// One catalog row: a model id and its input/output list price.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelEntry {
    /// Raw (un-namespaced) `provider/model` id, exactly as a client sends it.
    pub model: String,
    /// Prompt/input list price, micro-USD per 1,000,000 tokens.
    pub input_micro_usd_per_million: i64,
    /// Completion/output list price, micro-USD per 1,000,000 tokens.
    pub output_micro_usd_per_million: i64,
}

impl ModelEntry {
    fn price_row(&self) -> PriceRow {
        PriceRow {
            input_micro_usd_per_million: self.input_micro_usd_per_million,
            output_micro_usd_per_million: self.output_micro_usd_per_million,
        }
    }
}

/// The parsed, validated price-catalog config.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PriceCatalogConfig {
    /// Optional override for the safe-default price applied to unknown models.
    #[serde(default)]
    pub default: Option<PriceRow>,
    /// Per-model overrides overlaid on the embedded catalog.
    #[serde(default)]
    pub models: Vec<ModelEntry>,
}

impl PriceCatalogConfig {
    /// Validate every rate, id, and uniqueness invariant. Called by [`parse`];
    /// exposed so a config built in memory can be checked too.
    pub fn validate(&self) -> Result<(), CatalogConfigError> {
        if let Some(default) = self.default {
            default.validate("default")?;
        }
        let mut seen: HashSet<&str> = HashSet::new();
        for entry in &self.models {
            let id = entry.model.trim();
            if id.is_empty() {
                return Err(CatalogConfigError::EmptyModelId);
            }
            entry.price_row().validate(id)?;
            if !seen.insert(id) {
                return Err(CatalogConfigError::DuplicateModel(id.to_string()));
            }
        }
        Ok(())
    }

    /// Build a [`PriceTable`] by overlaying this config on the embedded catalog.
    ///
    /// The resolved default is this config's `default` if present, else the
    /// supplied `fallback_default`. The embedded catalog seeds the table, then
    /// each config model is inserted (adding or overriding). Unknown models
    /// still resolve to the resolved default.
    pub fn into_price_table(&self, fallback_default: ModelPrice) -> PriceTable {
        let default = self
            .default
            .map(PriceRow::to_model_price)
            .unwrap_or(fallback_default);
        let mut table = price_table(default);
        for entry in &self.models {
            table.insert(
                ModelId::namespace(entry.model.trim()),
                entry.price_row().to_model_price(),
            );
        }
        table
    }
}

/// Parse and validate a price-catalog config from a JSON string.
pub fn parse(json: &str) -> Result<PriceCatalogConfig, CatalogConfigError> {
    let config: PriceCatalogConfig =
        serde_json::from_str(json).map_err(|e| CatalogConfigError::Parse(e.to_string()))?;
    config.validate()?;
    Ok(config)
}

/// Read, parse, validate, and build a [`PriceTable`] from a JSON catalog file,
/// overlaid on the embedded catalog with `fallback_default` as the safe default.
///
/// This is the single entry point the gateway wires in when an operator points
/// `RELAY_PRICE_CATALOG_PATH` at a file. Any failure (missing file, bad JSON,
/// invalid price) is returned as [`CatalogConfigError`] so startup fails closed
/// rather than billing on an unvalidated table.
pub fn load_from_path(
    path: impl AsRef<Path>,
    fallback_default: ModelPrice,
) -> Result<PriceTable, CatalogConfigError> {
    let path = path.as_ref();
    let contents = std::fs::read_to_string(path).map_err(|e| CatalogConfigError::Io {
        path: path.display().to_string(),
        message: e.to_string(),
    })?;
    let config = parse(&contents)?;
    Ok(config.into_price_table(fallback_default))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::money::Micros;

    const USD_PER_MILLION: i64 = 1_000_000;

    fn default_price() -> ModelPrice {
        // Mirrors the gateway's conservative default ($10 / $30 per 1M).
        ModelPrice::new(10, 30)
    }

    fn model(raw: &str) -> ModelId {
        ModelId::namespace(raw)
    }

    #[test]
    fn parse_accepts_a_valid_config_and_prices_the_new_model() {
        let json = r#"
        {
          "models": [
            { "model": "acme/foo-1", "input_micro_usd_per_million": 5000000, "output_micro_usd_per_million": 15000000 }
          ]
        }"#;
        let config = parse(json).expect("valid config");
        let table = config.into_price_table(default_price());

        // acme/foo-1 is not in the embedded catalog; the config adds it.
        assert!(table.is_known(&model("acme/foo-1")));
        // $5 / $15 per 1M -> 5 / 15 micro-USD per token.
        assert_eq!(
            table.price_for(&model("acme/foo-1")),
            ModelPrice::new(5, 15)
        );
        // 1000 * 5 + 500 * 15 = 5000 + 7500 = 12_500
        assert_eq!(
            table.cost(&model("acme/foo-1"), 1000, 500),
            Micros::new(12_500)
        );
    }

    #[test]
    fn config_overrides_an_embedded_catalog_model() {
        // gpt-4o ships as (3, 10) in the embedded catalog; override it.
        let json = r#"
        {
          "models": [
            { "model": "openai/gpt-4o", "input_micro_usd_per_million": 1000000, "output_micro_usd_per_million": 2000000 }
          ]
        }"#;
        let table = parse(json).unwrap().into_price_table(default_price());
        assert_eq!(
            table.price_for(&model("openai/gpt-4o")),
            ModelPrice::new(1, 2)
        );
        // A catalog model NOT overridden keeps its embedded price.
        assert_eq!(
            table.price_for(&model("anthropic/claude-3-opus")),
            ModelPrice::new(15, 75)
        );
    }

    #[test]
    fn unknown_model_falls_back_to_the_supplied_default() {
        let table = parse("{}").unwrap().into_price_table(default_price());
        let unknown = model("nobody/knows-me");
        assert!(!table.is_known(&unknown));
        assert_eq!(table.price_for(&unknown), default_price());
    }

    #[test]
    fn config_default_overrides_the_fallback_default() {
        let json = r#"
        {
          "default": { "input_micro_usd_per_million": 20000000, "output_micro_usd_per_million": 40000000 }
        }"#;
        let table = parse(json).unwrap().into_price_table(default_price());
        let unknown = model("nobody/knows-me");
        // Resolved default is the config's $20 / $40, not the fallback $10 / $30.
        assert_eq!(table.price_for(&unknown), ModelPrice::new(20, 40));
    }

    #[test]
    fn parse_rejects_a_negative_price() {
        let json = r#"
        {
          "models": [
            { "model": "acme/foo-1", "input_micro_usd_per_million": -1, "output_micro_usd_per_million": 1000000 }
          ]
        }"#;
        let err = parse(json).unwrap_err();
        assert_eq!(
            err,
            CatalogConfigError::NegativePrice {
                model: "acme/foo-1".to_string(),
                field: "input",
                value: -1,
            }
        );
    }

    #[test]
    fn parse_rejects_a_duplicate_model_id() {
        let json = r#"
        {
          "models": [
            { "model": "acme/foo-1", "input_micro_usd_per_million": 1000000, "output_micro_usd_per_million": 1000000 },
            { "model": "acme/foo-1", "input_micro_usd_per_million": 2000000, "output_micro_usd_per_million": 2000000 }
          ]
        }"#;
        assert_eq!(
            parse(json).unwrap_err(),
            CatalogConfigError::DuplicateModel("acme/foo-1".to_string())
        );
    }

    #[test]
    fn parse_rejects_an_empty_model_id() {
        let json = r#"
        {
          "models": [
            { "model": "   ", "input_micro_usd_per_million": 1000000, "output_micro_usd_per_million": 1000000 }
          ]
        }"#;
        assert_eq!(parse(json).unwrap_err(), CatalogConfigError::EmptyModelId);
    }

    #[test]
    fn parse_rejects_unknown_fields() {
        let json = r#"{ "modles": [] }"#;
        assert!(matches!(
            parse(json).unwrap_err(),
            CatalogConfigError::Parse(_)
        ));
    }

    #[test]
    fn parse_rejects_malformed_json() {
        assert!(matches!(
            parse("not json").unwrap_err(),
            CatalogConfigError::Parse(_)
        ));
    }

    #[test]
    fn load_from_path_reads_and_builds_a_table() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("relay-price-catalog-{}.json", std::process::id()));
        let json = format!(
            r#"{{ "models": [ {{ "model": "acme/bar-9", "input_micro_usd_per_million": {}, "output_micro_usd_per_million": {} }} ] }}"#,
            2 * USD_PER_MILLION,
            6 * USD_PER_MILLION
        );
        std::fs::write(&path, json).expect("write temp catalog");

        let table = load_from_path(&path, default_price()).expect("load catalog");
        assert_eq!(table.price_for(&model("acme/bar-9")), ModelPrice::new(2, 6));

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn load_from_path_reports_a_missing_file_as_io_error() {
        let missing = std::env::temp_dir().join("relay-price-catalog-does-not-exist-xyz.json");
        let err = load_from_path(&missing, default_price()).unwrap_err();
        assert!(matches!(err, CatalogConfigError::Io { .. }));
    }
}
