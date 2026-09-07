//! Normalized usage parsing for native and OpenAI-compatible responses.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use ts_rs::TS;

use crate::money::{Microdollars, MoneyError};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub provider_cost_micros: Option<Microdollars>,
}

impl Usage {
    pub fn from_response(body: &serde_json::Value) -> Result<Option<Self>, UsageParseError> {
        let Some(raw) = body.get("usage") else {
            return Ok(None);
        };
        if raw.is_null() {
            return Ok(None);
        }
        let input_tokens = token(raw, "prompt_tokens")
            .or_else(|| token(raw, "input_tokens"))
            .unwrap_or(0);
        let output_tokens = token(raw, "completion_tokens")
            .or_else(|| token(raw, "output_tokens"))
            .unwrap_or(0);
        let total_tokens = token(raw, "total_tokens")
            .unwrap_or_else(|| input_tokens.saturating_add(output_tokens));
        let provider_cost_micros = raw.get("tensorzero_cost").map(parse_cost).transpose()?;
        Ok(Some(Self {
            input_tokens,
            output_tokens,
            total_tokens,
            provider_cost_micros,
        }))
    }
}

fn token(value: &serde_json::Value, key: &str) -> Option<u64> {
    value.get(key)?.as_u64()
}

fn parse_cost(value: &serde_json::Value) -> Result<Microdollars, UsageParseError> {
    let raw = match value {
        serde_json::Value::Number(number) => number.to_string(),
        serde_json::Value::String(value) => value.clone(),
        _ => return Err(UsageParseError::InvalidProviderCost),
    };
    Microdollars::from_usd_decimal(&raw).map_err(UsageParseError::Money)
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UsageParseError {
    #[error("provider cost is not a JSON number or decimal string")]
    InvalidProviderCost,
    #[error(transparent)]
    Money(#[from] MoneyError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_openai_and_native_token_names() {
        let openai = serde_json::json!({"usage": {
            "prompt_tokens": 10, "completion_tokens": 4, "tensorzero_cost": 0.00125
        }});
        let usage = Usage::from_response(&openai).unwrap().unwrap();
        assert_eq!(usage.input_tokens, 10);
        assert_eq!(usage.output_tokens, 4);
        assert_eq!(usage.total_tokens, 14);
        assert_eq!(usage.provider_cost_micros, Some(Microdollars::new(1_250)));

        let native = serde_json::json!({"usage": {"input_tokens": 3, "output_tokens": 2}});
        assert_eq!(
            Usage::from_response(&native).unwrap().unwrap().total_tokens,
            5
        );
    }
}
