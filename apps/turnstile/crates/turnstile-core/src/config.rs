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
        validate_payee_address(&payee)?;

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

/// The human-readable part every Mint receive address uses (`mint-core`'s
/// `HRP_STR`, duplicated here rather than depending on `mint-core` just for
/// one constant — Turnstile stays decoupled from Mint's crate graph).
const PAYEE_HRP: &str = "ait";

/// Reject a `TURNSTILE_PAYEE` that is not a real, checksummed `ait1…` bech32
/// address — fail fast, same spirit as the price/TTL checks above.
///
/// Without this, a non-address string (a copy-pasted placeholder like
/// `ait1qprovider`, or any other typo) passes `require()` silently: the
/// server starts, serves the catalog, and issues real `402` challenges, and
/// only fails — with an opaque `mint error (status 400, code 'bad_address')`
/// — the first time a successful call reaches settlement and this module
/// tries to capture to it. Confirmed live against a real mint-server: the
/// README's own former quickstart value (`ait1qprovider`) is exactly this
/// case — syntactically address-shaped, not actually valid bech32 — so
/// nobody following that command could ever complete a real payment.
fn validate_payee_address(payee: &str) -> Result<(), TurnstileError> {
    let (hrp, _payload) = bech32::decode(payee).map_err(|e| {
        TurnstileError::Config(format!(
            "TURNSTILE_PAYEE {payee:?} is not a valid ait1… bech32 address ({e}) — mint a real \
             one first: POST /v1/accounts then POST /v1/accounts/:id/addresses on the \
             mint-server, and use the returned \"address\" value"
        ))
    })?;
    if hrp.as_str() != PAYEE_HRP {
        return Err(TurnstileError::Config(format!(
            "TURNSTILE_PAYEE {payee:?} has human-readable part {:?}, expected {PAYEE_HRP:?} \
             (a Mint receive address)",
            hrp.as_str()
        )));
    }
    Ok(())
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

        // A complete, valid environment parses into integer microdollars. The
        // payee below is a REAL bech32-valid `ait1…` address (minted live
        // against a running mint-server while hunting the bug this guards —
        // see `validate_payee_address`'s doc comment), not a hand-typed
        // placeholder like the old fixture (`ait1qpayee`) that this test used
        // to accept despite being exactly as invalid as the bug it now catches.
        const REAL_PAYEE_ADDRESS: &str = "ait1qw3wvg6lhfz9eu3g4srleu2f028sxru83w";
        std::env::set_var("MINT_SERVER_URL", "http://mint.internal");
        std::env::set_var("TURNSTILE_API_KEY", "secret");
        std::env::set_var("TURNSTILE_PAYEE", REAL_PAYEE_ADDRESS);
        std::env::set_var("TURNSTILE_PRICE_MICROS", "4200");
        let cfg = TurnstileConfig::from_env().unwrap();
        assert_eq!(cfg.mint_server_url, "http://mint.internal");
        assert_eq!(cfg.payee, REAL_PAYEE_ADDRESS);
        assert_eq!(cfg.price_micros, AmountMicros(4200));
        assert_eq!(cfg.currency, "AIT"); // default
        assert_eq!(cfg.challenge_ttl_ms, DEFAULT_CHALLENGE_TTL_MS);

        // A non-integer price is rejected (no floats, ever).
        std::env::set_var("TURNSTILE_PRICE_MICROS", "4.2");
        assert!(matches!(
            TurnstileConfig::from_env(),
            Err(TurnstileError::Config(_))
        ));
        std::env::set_var("TURNSTILE_PRICE_MICROS", "4200");

        // Regression: a syntactically address-shaped but NOT actually valid
        // bech32 string (no real checksum) must fail fast at startup instead
        // of starting a server that can issue 402s all day and only ever
        // fail with a cryptic `bad_address` the first time anyone pays it.
        // This exact value is what this crate's own README quickstart used
        // to document before the fix.
        std::env::set_var("TURNSTILE_PAYEE", "ait1qprovider");
        assert!(matches!(
            TurnstileConfig::from_env(),
            Err(TurnstileError::Config(msg)) if msg.contains("TURNSTILE_PAYEE")
        ));

        // Regression: right checksum, wrong human-readable part (not a Mint
        // address at all, e.g. a Bitcoin bech32 address) is rejected too.
        std::env::set_var(
            "TURNSTILE_PAYEE",
            "bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4",
        );
        assert!(matches!(
            TurnstileConfig::from_env(),
            Err(TurnstileError::Config(msg)) if msg.contains("human-readable part")
        ));

        for k in keys {
            std::env::remove_var(k);
        }
    }
}
