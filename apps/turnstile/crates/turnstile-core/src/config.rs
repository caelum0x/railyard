//! Fail-fast configuration: environment → [`TurnstileConfig`].
//!
//! Per the security rules NOTHING here is hardcoded — the mint-server URL, API
//! key, payee address and price are read from the environment at startup and the
//! process refuses to start if a required value is missing or malformed.

use aitp_client::AmountMicros;

use crate::error::TurnstileError;

/// Default challenge time-to-live if `TURNSTILE_CHALLENGE_TTL_MS` is unset.
const DEFAULT_CHALLENGE_TTL_MS: i64 = 120_000;
/// Default settlement currency (SPEC: always `"AIT"`).
const DEFAULT_CURRENCY: &str = "AIT";

/// Everything Turnstile needs to gate a route and settle against a live mint.
#[derive(Debug, Clone)]
pub struct TurnstileConfig {
    /// Base URL of the mint-server this verifier forwards authorizations to.
    pub mint_server_url: String,
    /// Mint-server API key sent as `X-API-Key` on every protected route.
    pub api_key: String,
    /// Provider receive address (`ait1…`) placed in every challenge `payee`.
    pub payee: String,
    /// Price per protected call, in integer microdollars (never floats).
    pub price_micros: AmountMicros,
    /// Settlement currency, always `"AIT"` today.
    pub currency: String,
    /// How long an issued challenge/nonce stays valid, in milliseconds.
    pub challenge_ttl_ms: i64,
}

impl TurnstileConfig {
    /// Build from the process environment, failing fast on the first problem.
    ///
    /// Required: `MINT_SERVER_URL`, `TURNSTILE_API_KEY`, `TURNSTILE_PAYEE`,
    /// `TURNSTILE_PRICE_MICROS`. Optional: `TURNSTILE_CURRENCY` (default `AIT`),
    /// `TURNSTILE_CHALLENGE_TTL_MS` (default 120000).
    pub fn from_env() -> Result<Self, TurnstileError> {
        let mint_server_url = require("MINT_SERVER_URL")?;
        let api_key = require("TURNSTILE_API_KEY")?;
        let payee = require("TURNSTILE_PAYEE")?;

        let price_raw = require("TURNSTILE_PRICE_MICROS")?;
        let price_micros = price_raw.trim().parse::<i64>().map_err(|e| {
            TurnstileError::Config(format!("TURNSTILE_PRICE_MICROS is not an integer: {e}"))
        })?;
        if price_micros < 0 {
            return Err(TurnstileError::Config(
                "TURNSTILE_PRICE_MICROS must be non-negative".to_string(),
            ));
        }

        let currency =
            optional("TURNSTILE_CURRENCY").unwrap_or_else(|| DEFAULT_CURRENCY.to_string());

        let challenge_ttl_ms = match optional("TURNSTILE_CHALLENGE_TTL_MS") {
            Some(raw) => raw.trim().parse::<i64>().map_err(|e| {
                TurnstileError::Config(format!("TURNSTILE_CHALLENGE_TTL_MS is not an integer: {e}"))
            })?,
            None => DEFAULT_CHALLENGE_TTL_MS,
        };
        if challenge_ttl_ms <= 0 {
            return Err(TurnstileError::Config(
                "TURNSTILE_CHALLENGE_TTL_MS must be positive".to_string(),
            ));
        }

        Ok(Self {
            mint_server_url,
            api_key,
            payee,
            price_micros: AmountMicros::new(price_micros),
            currency,
            challenge_ttl_ms,
        })
    }
}

/// Read a required env var, mapping absence/empty to a config error.
fn require(key: &'static str) -> Result<String, TurnstileError> {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => Ok(v),
        _ => Err(TurnstileError::Config(format!("{key} must be set"))),
    }
}

/// Read an optional env var, treating empty as absent.
fn optional(key: &'static str) -> Option<String> {
    std::env::var(key).ok().filter(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All config-var mutations live in ONE test so they run sequentially and
    /// never race the (env-free) tests in other modules.
    #[test]
    fn from_env_requires_all_secrets_and_parses_price() {
        let keys = [
            "MINT_SERVER_URL",
            "TURNSTILE_API_KEY",
            "TURNSTILE_PAYEE",
            "TURNSTILE_PRICE_MICROS",
            "TURNSTILE_CURRENCY",
            "TURNSTILE_CHALLENGE_TTL_MS",
        ];
        for k in keys {
            std::env::remove_var(k);
        }

        // Missing everything → fail fast on the first required var.
        assert!(matches!(
            TurnstileConfig::from_env(),
            Err(TurnstileError::Config(_))
        ));

        // A complete, valid environment parses into integer microdollars.
        std::env::set_var("MINT_SERVER_URL", "http://mint.internal");
        std::env::set_var("TURNSTILE_API_KEY", "secret");
        std::env::set_var("TURNSTILE_PAYEE", "ait1qpayee");
        std::env::set_var("TURNSTILE_PRICE_MICROS", "4200");
        let cfg = TurnstileConfig::from_env().unwrap();
        assert_eq!(cfg.mint_server_url, "http://mint.internal");
        assert_eq!(cfg.price_micros, AmountMicros(4200));
        assert_eq!(cfg.currency, "AIT"); // default
        assert_eq!(cfg.challenge_ttl_ms, DEFAULT_CHALLENGE_TTL_MS);

        // A non-integer price is rejected (no floats, ever).
        std::env::set_var("TURNSTILE_PRICE_MICROS", "4.2");
        assert!(matches!(
            TurnstileConfig::from_env(),
            Err(TurnstileError::Config(_))
        ));

        for k in keys {
            std::env::remove_var(k);
        }
    }
}
