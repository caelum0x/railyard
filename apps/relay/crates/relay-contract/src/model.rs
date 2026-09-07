//! Stable model selector vocabulary across Relay clients.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use ts_rs::TS;

pub const OPENROUTER_CATALOG_PREFIX: &str = "openrouter/";
const MODEL_SELECTOR_PREFIX: &str = "tensorzero::model_name::";
const FUNCTION_SELECTOR_PREFIX: &str = "tensorzero::function_name::";
const VARIANT_SELECTOR: &str = "::variant_name::";

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(transparent)]
pub struct ModelSelector(String);

impl ModelSelector {
    pub fn concrete(provider: &str, model: &str) -> Result<Self, ModelSelectorError> {
        validate_segment("provider", provider)?;
        validate_segment("model", model)?;
        Ok(Self(format!("{MODEL_SELECTOR_PREFIX}{provider}::{model}")))
    }

    pub fn function(function: &str) -> Result<Self, ModelSelectorError> {
        validate_segment("function", function)?;
        Ok(Self(format!("{FUNCTION_SELECTOR_PREFIX}{function}")))
    }

    pub fn function_variant(function: &str, variant: &str) -> Result<Self, ModelSelectorError> {
        validate_segment("function", function)?;
        validate_segment("variant", variant)?;
        Ok(Self(format!(
            "{FUNCTION_SELECTOR_PREFIX}{function}{VARIANT_SELECTOR}{variant}"
        )))
    }

    pub fn parse(value: impl Into<String>) -> Result<Self, ModelSelectorError> {
        let value = value.into();
        if let Some(tail) = value.strip_prefix(MODEL_SELECTOR_PREFIX) {
            let Some((provider, model)) = tail.split_once("::") else {
                return Err(ModelSelectorError::Malformed);
            };
            validate_segment("provider", provider)?;
            validate_segment("model", model)?;
        } else if let Some(tail) = value.strip_prefix(FUNCTION_SELECTOR_PREFIX) {
            let (function, variant) = tail
                .split_once(VARIANT_SELECTOR)
                .map_or((tail, None), |(function, variant)| {
                    (function, Some(variant))
                });
            validate_segment("function", function)?;
            if let Some(variant) = variant {
                validate_segment("variant", variant)?;
            }
        } else {
            return Err(ModelSelectorError::UnknownNamespace);
        }
        Ok(Self(value))
    }

    pub fn from_catalog_id(catalog_id: &str) -> Result<Self, ModelSelectorError> {
        let raw = catalog_id
            .strip_prefix(OPENROUTER_CATALOG_PREFIX)
            .unwrap_or(catalog_id);
        let Some((provider, model)) = raw.split_once('/') else {
            return Err(ModelSelectorError::Malformed);
        };
        Self::concrete(provider, model)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ModelSelector {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn validate_segment(name: &'static str, value: &str) -> Result<(), ModelSelectorError> {
    if value.trim().is_empty() || value.chars().any(char::is_whitespace) {
        return Err(ModelSelectorError::InvalidSegment(name));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum ModelSelectorError {
    #[error("model selector has an unknown namespace")]
    UnknownNamespace,
    #[error("model selector is malformed")]
    Malformed,
    #[error("model selector has an invalid {0} segment")]
    InvalidSegment(&'static str),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_model_becomes_tensorzero_concrete_selector() {
        let selector = ModelSelector::from_catalog_id("openrouter/openai/gpt-4o").unwrap();
        assert_eq!(selector.as_str(), "tensorzero::model_name::openai::gpt-4o");
    }

    #[test]
    fn function_variant_round_trips() {
        let selector = ModelSelector::function_variant("summarize", "fast").unwrap();
        assert_eq!(ModelSelector::parse(selector.to_string()), Ok(selector));
    }
}
