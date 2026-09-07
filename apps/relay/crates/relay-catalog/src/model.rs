use std::collections::HashSet;

use relay_contract::Microdollars;
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::CATALOG_SCHEMA_VERSION;
use crate::error::{CatalogError, Result};

const TOKENS_PER_MILLION: i128 = 1_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum PricingSource {
    ProviderList,
    Contract,
    OperatorOverride,
    ConservativeFallback,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ModelCapabilities {
    #[serde(default)]
    pub chat: bool,
    #[serde(default)]
    pub embeddings: bool,
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub json_schema: bool,
    #[serde(default)]
    pub streaming: bool,
    #[serde(default)]
    pub batch: bool,
}

impl ModelCapabilities {
    pub const CHAT: Self = Self {
        chat: true,
        embeddings: false,
        tools: true,
        vision: false,
        json_schema: true,
        streaming: true,
        batch: true,
    };

    pub const fn any(self) -> bool {
        self.chat || self.embeddings
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct ModelPrice {
    pub input_micro_usd_per_million: i64,
    pub output_micro_usd_per_million: i64,
    pub source: PricingSource,
}

impl ModelPrice {
    pub const fn new(input: i64, output: i64, source: PricingSource) -> Self {
        Self {
            input_micro_usd_per_million: input,
            output_micro_usd_per_million: output,
            source,
        }
    }

    pub fn quote(self, input_tokens: u64, output_tokens: u64) -> Result<Microdollars> {
        let input = i128::from(input_tokens)
            .checked_mul(i128::from(self.input_micro_usd_per_million))
            .ok_or(CatalogError::Overflow)?;
        let output = i128::from(output_tokens)
            .checked_mul(i128::from(self.output_micro_usd_per_million))
            .ok_or(CatalogError::Overflow)?;
        let numerator = input.checked_add(output).ok_or(CatalogError::Overflow)?;
        let rounded = if numerator == 0 {
            0
        } else {
            numerator
                .checked_add(TOKENS_PER_MILLION - 1)
                .ok_or(CatalogError::Overflow)?
                / TOKENS_PER_MILLION
        };
        i64::try_from(rounded)
            .map(Microdollars::new)
            .map_err(|_| CatalogError::Overflow)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct CatalogModel {
    pub id: String,
    pub provider: String,
    pub display_name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub capabilities: ModelCapabilities,
    pub pricing: ModelPrice,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub context_window_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub max_output_tokens: Option<u64>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct CatalogSnapshot {
    pub schema_version: u16,
    pub revision: String,
    pub generated_at_ms: i64,
    pub models: Vec<CatalogModel>,
}

impl CatalogSnapshot {
    pub fn new(
        revision: impl Into<String>,
        generated_at_ms: i64,
        models: Vec<CatalogModel>,
    ) -> Self {
        Self {
            schema_version: CATALOG_SCHEMA_VERSION,
            revision: revision.into(),
            generated_at_ms,
            models,
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema_version != CATALOG_SCHEMA_VERSION {
            return Err(CatalogError::UnsupportedSchemaVersion {
                expected: CATALOG_SCHEMA_VERSION,
                actual: self.schema_version,
            });
        }
        if self.revision.trim().is_empty() {
            return Err(CatalogError::InvalidRevision);
        }
        if self.generated_at_ms < 0 {
            return Err(CatalogError::InvalidGeneratedAt);
        }
        let mut identities = HashSet::new();
        for model in &self.models {
            validate_id(&model.id)?;
            let canonical_id = canonical_catalog_id(&model.id);
            if !identities.insert(canonical_id.clone()) {
                return Err(CatalogError::DuplicateIdentity(model.id.clone()));
            }
            let expected_provider = if is_dynamic_route_id(&canonical_id) {
                "tensorzero"
            } else {
                canonical_id
                    .split_once('/')
                    .map_or("", |(provider, _)| provider)
            };
            if model.provider != expected_provider {
                return Err(CatalogError::InvalidProvider {
                    model: model.id.clone(),
                    expected: expected_provider.into(),
                });
            }
            if model.display_name.trim().is_empty() {
                return Err(CatalogError::InvalidDisplayName(model.id.clone()));
            }
            if model.pricing.input_micro_usd_per_million < 0 {
                return Err(CatalogError::NegativeRate {
                    model: model.id.clone(),
                    leg: "input",
                });
            }
            if model.pricing.output_micro_usd_per_million < 0 {
                return Err(CatalogError::NegativeRate {
                    model: model.id.clone(),
                    leg: "output",
                });
            }
            if !model.capabilities.any() {
                return Err(CatalogError::NoCapabilities(model.id.clone()));
            }
            for alias in &model.aliases {
                validate_id(alias)?;
                if !identities.insert(canonical_catalog_id(alias)) {
                    return Err(CatalogError::DuplicateIdentity(alias.clone()));
                }
            }
        }
        Ok(())
    }
}

pub(crate) fn canonical_catalog_id(raw: &str) -> String {
    const MODEL_PREFIX: &str = "tensorzero::model_name::";
    const FUNCTION_PREFIX: &str = "tensorzero::function_name::";
    let raw = raw.strip_prefix("openrouter/").unwrap_or(raw);
    if let Some(function) = raw.strip_prefix(FUNCTION_PREFIX) {
        let function = function
            .split_once("::variant_name::")
            .map_or(function, |(function, _variant)| function);
        return format!("{FUNCTION_PREFIX}{function}");
    }
    if let Some(model) = raw.strip_prefix(MODEL_PREFIX) {
        return match model.split_once("::") {
            Some((provider, model)) if !provider.is_empty() && !model.is_empty() => {
                format!("{provider}/{model}")
            }
            _ => format!("{MODEL_PREFIX}{model}"),
        };
    }
    match raw.split_once("::") {
        Some((provider, model)) => format!("{provider}/{model}"),
        None => raw.to_string(),
    }
}

fn validate_id(value: &str) -> Result<()> {
    if value.trim() != value
        || value.is_empty()
        || value.chars().any(char::is_whitespace)
        || (!value.contains('/') && !is_dynamic_route_id(value))
        || (is_dynamic_route_id(value) && dynamic_route_name(value).is_none_or(str::is_empty))
    {
        return Err(CatalogError::InvalidModelId(value.into()));
    }
    Ok(())
}

fn is_function_id(value: &str) -> bool {
    value.starts_with("tensorzero::function_name::")
}

fn is_named_model_id(value: &str) -> bool {
    value.starts_with("tensorzero::model_name::")
}

fn is_dynamic_route_id(value: &str) -> bool {
    is_function_id(value) || is_named_model_id(value)
}

fn dynamic_route_name(value: &str) -> Option<&str> {
    value
        .strip_prefix("tensorzero::function_name::")
        .or_else(|| value.strip_prefix("tensorzero::model_name::"))
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    fn model(id: &str) -> CatalogModel {
        let provider = id.split_once('/').unwrap().0;
        CatalogModel {
            id: id.into(),
            provider: provider.into(),
            display_name: id.into(),
            aliases: Vec::new(),
            capabilities: ModelCapabilities::CHAT,
            pricing: ModelPrice::new(1, 1, PricingSource::OperatorOverride),
            context_window_tokens: None,
            max_output_tokens: None,
            enabled: true,
        }
    }

    #[test]
    fn rejects_unknown_schema_and_invalid_metadata() {
        let mut snapshot = CatalogSnapshot::new("revision", 0, vec![model("acme/model")]);
        snapshot.schema_version += 1;
        assert!(matches!(
            snapshot.validate(),
            Err(CatalogError::UnsupportedSchemaVersion { .. })
        ));

        snapshot.schema_version = CATALOG_SCHEMA_VERSION;
        snapshot.revision.clear();
        assert_eq!(snapshot.validate(), Err(CatalogError::InvalidRevision));

        snapshot.revision = "revision".into();
        snapshot.generated_at_ms = -1;
        assert_eq!(snapshot.validate(), Err(CatalogError::InvalidGeneratedAt));
    }

    #[test]
    fn rejects_canonical_identity_collision_and_provider_mismatch() {
        let mut first = model("openai/gpt-4o");
        first.aliases.push("openrouter/openai/gpt-4o".into());
        let snapshot = CatalogSnapshot::new("revision", 0, vec![first]);
        assert_eq!(
            snapshot.validate(),
            Err(CatalogError::DuplicateIdentity(
                "openrouter/openai/gpt-4o".into()
            ))
        );

        let mut wrong_provider = model("acme/model");
        wrong_provider.provider = "other".into();
        let snapshot = CatalogSnapshot::new("revision", 0, vec![wrong_provider]);
        assert_eq!(
            snapshot.validate(),
            Err(CatalogError::InvalidProvider {
                model: "acme/model".into(),
                expected: "acme".into()
            })
        );
    }

    #[test]
    fn function_identity_is_first_class_and_variants_canonicalize() {
        let mut function = model("acme/placeholder");
        function.id = "tensorzero::function_name::write_copy".into();
        function.provider = "tensorzero".into();
        function.display_name = "Write copy".into();
        let snapshot = CatalogSnapshot::new("revision", 0, vec![function]);
        assert_eq!(snapshot.validate(), Ok(()));
        assert_eq!(
            canonical_catalog_id("tensorzero::function_name::write_copy::variant_name::quality"),
            "tensorzero::function_name::write_copy"
        );
    }

    #[test]
    fn configured_model_identity_is_first_class() {
        let mut configured = model("acme/placeholder");
        configured.id = "tensorzero::model_name::production-chat".into();
        configured.provider = "tensorzero".into();
        configured.display_name = "Production chat".into();
        let snapshot = CatalogSnapshot::new("revision", 0, vec![configured]);
        assert_eq!(snapshot.validate(), Ok(()));
        assert_eq!(
            canonical_catalog_id("tensorzero::model_name::production-chat"),
            "tensorzero::model_name::production-chat"
        );
        assert_eq!(
            canonical_catalog_id("tensorzero::model_name::openai::gpt-4o"),
            "openai/gpt-4o"
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PriceQuote {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub amount_micros: Microdollars,
    pub pricing: ModelPrice,
    pub catalog_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PriceQuoteRequest {
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
}
