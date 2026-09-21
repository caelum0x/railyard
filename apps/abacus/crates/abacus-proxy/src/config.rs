//! Fail-fast startup configuration, loaded from the environment. NO secret is
//! ever hardcoded — every value is read from an env var and validated before the
//! server binds. A missing/empty required var is a fatal [`AbacusError::Config`].

use abacus_core::error::AbacusError;

/// The resolved runtime configuration for one Abacus deployment.
#[derive(Debug, Clone)]
pub struct Config {
    /// Listen address (`ABACUS_BIND_ADDR`, default `0.0.0.0:8100`).
    pub bind_addr: String,
    /// Base URL of the live mint-server (`MINT_SERVER_URL`).
    pub mint_server_url: String,
    /// Shared `X-API-Key` secret for the mint-server (`MINT_API_KEY`).
    pub mint_api_key: String,
    /// Ingress `X-API-Key` secret agents must present to Abacus (`ABACUS_API_KEY`).
    pub abacus_api_key: String,
    /// Base URL of the real upstream provider (`UPSTREAM_BASE_URL`).
    pub upstream_base_url: String,
    /// Provider key used for pricing-table lookup (`ABACUS_PROVIDER`, default
    /// `openai`).
    pub provider: String,
    /// Upstream path requests are forwarded to (`UPSTREAM_PATH`, default
    /// `/v1/chat/completions`).
    pub upstream_path: String,
    /// Optional multi-provider pricing config file (`ABACUS_PRICING_CONFIG`). When
    /// set, its entries overlay the built-in table so any provider/model the
    /// operator prices is honored; when absent, only the built-in OpenAI rates
    /// apply and other models fail with `model_not_priced`.
    pub pricing_config_path: Option<String>,
    /// Declarative agent/budget policy file (`ABACUS_AGENT_CONFIG`).
    pub agent_config_path: String,
    /// Append-only settled receipt log (`ABACUS_RECEIPT_LOG`).
    pub receipt_log_path: String,
    /// Atomic JSON snapshot of open Mint holds (`ABACUS_HOLD_STORE`).
    pub hold_store_path: String,
    /// Shared Postgres recovery state (`ABACUS_DATABASE_URL`). When absent,
    /// Abacus is explicitly limited to a single replica and uses the JSON file.
    pub database_url: Option<String>,
    /// Age after which a still-open hold is considered orphaned.
    pub hold_ttl_ms: i64,
    /// Interval between orphan sweeps.
    pub hold_reaper_interval_ms: u64,
}

/// Read a required, non-empty env var or fail fast with a config error.
fn require(key: &str) -> Result<String, AbacusError> {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        _ => Err(AbacusError::Config(format!("missing or empty {key}"))),
    }
}

/// Read an optional env var, falling back to `default` when unset/empty.
fn optional(key: &str, default: &str) -> String {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => default.to_string(),
    }
}

fn optional_value(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn positive_i64(key: &str, default: i64) -> Result<i64, AbacusError> {
    let raw = optional(key, &default.to_string());
    raw.parse::<i64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| AbacusError::Config(format!("{key} must be a positive integer")))
}

impl Config {
    /// Load + validate the configuration from the process environment.
    pub fn from_env() -> Result<Self, AbacusError> {
        Ok(Config {
            bind_addr: optional("ABACUS_BIND_ADDR", "0.0.0.0:8100"),
            mint_server_url: require("MINT_SERVER_URL")?,
            mint_api_key: require("MINT_API_KEY")?,
            abacus_api_key: require("ABACUS_API_KEY")?,
            upstream_base_url: require("UPSTREAM_BASE_URL")?,
            provider: optional("ABACUS_PROVIDER", "openai"),
            upstream_path: optional("UPSTREAM_PATH", "/v1/chat/completions"),
            pricing_config_path: optional_value("ABACUS_PRICING_CONFIG"),
            agent_config_path: require("ABACUS_AGENT_CONFIG")?,
            receipt_log_path: optional("ABACUS_RECEIPT_LOG", "./abacus-receipts.jsonl"),
            hold_store_path: optional("ABACUS_HOLD_STORE", "./abacus-open-holds.json"),
            database_url: optional_value("ABACUS_DATABASE_URL"),
            hold_ttl_ms: positive_i64("ABACUS_HOLD_TTL_MS", 15 * 60 * 1_000)?,
            hold_reaper_interval_ms: positive_i64("ABACUS_HOLD_REAPER_INTERVAL_MS", 30_000)? as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn require_rejects_empty() {
        assert!(matches!(
            require("__ABACUS_NEVER_SET__"),
            Err(AbacusError::Config(_))
        ));
    }

    #[test]
    fn optional_falls_back() {
        assert_eq!(optional("__ABACUS_NEVER_SET__", "def"), "def");
    }
}
