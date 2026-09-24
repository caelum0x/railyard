//! x402 configuration: environment → [`X402Config`].
//!
//! x402 is OFF unless `TURNSTILE_X402_PAY_TO` is set. When it is set every other
//! value is validated up front and the process refuses to start on a missing or
//! malformed value, with an error that names the variable to fix.
//!
//! | Variable | Required | Meaning |
//! |---|---|---|
//! | `TURNSTILE_X402_PAY_TO` | yes (enables x402) | EVM address that receives USDC |
//! | `TURNSTILE_X402_NETWORK` | no, default `base-sepolia` | facilitator network id |
//! | `TURNSTILE_X402_FACILITATOR_URL` | on mainnet | facilitator base URL (`/verify`, `/settle`) |
//! | `TURNSTILE_X402_FACILITATOR_AUTH` | no | full `Authorization` header value for the facilitator |
//! | `TURNSTILE_X402_ASSET` | off Base | ERC-20 contract of the asset (USDC on Base/Base Sepolia by default) |
//! | `TURNSTILE_X402_ASSET_NAME` / `_ASSET_VERSION` | no | EIP-712 domain (`USD Coin` / `2`) |
//! | `TURNSTILE_X402_MAX_TIMEOUT_SECS` | no, default 60 | `maxTimeoutSeconds` advertised |
//! | `TURNSTILE_X402_MINT_ACCOUNT` | no | Mint account credited with every settled payment |
//! | `TURNSTILE_PUBLIC_URL` | no | absolute origin used for `resource` URLs |

use crate::error::TurnstileError;

/// Native USDC on Base mainnet.
pub const USDC_BASE: &str = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913";
/// Circle test USDC on Base Sepolia.
pub const USDC_BASE_SEPOLIA: &str = "0x036CbD53842c5426634e7929541eC2318f3dCF7e";
/// Public keyless facilitator for testnets (x402 Foundation).
pub const TESTNET_FACILITATOR: &str = "https://x402.org/facilitator";

const DEFAULT_NETWORK: &str = "base-sepolia";
const DEFAULT_ASSET_NAME: &str = "USD Coin";
const DEFAULT_ASSET_VERSION: &str = "2";
const DEFAULT_MAX_TIMEOUT_SECS: u64 = 60;

/// Everything the x402 rail needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct X402Config {
    pub pay_to: String,
    pub network: String,
    pub facilitator_url: String,
    pub facilitator_auth: Option<String>,
    pub asset: String,
    pub asset_name: String,
    pub asset_version: String,
    pub max_timeout_seconds: u64,
    /// Mint account (`acct_…`) credited with each settled x402 payment. When
    /// unset, settlements are journaled in Turnstile only.
    pub mint_account: Option<String>,
    /// Absolute origin (`https://tools.example.com`) for `resource` URLs.
    pub public_url: Option<String>,
}

impl X402Config {
    /// `Ok(None)` when x402 is disabled (no `TURNSTILE_X402_PAY_TO`).
    pub fn from_env() -> Result<Option<Self>, TurnstileError> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }

    /// Same as [`from_env`](Self::from_env) over an injectable lookup (tests).
    pub fn from_lookup<F>(lookup: F) -> Result<Option<Self>, TurnstileError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let get = |key: &str| {
            lookup(key)
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
        };
        let Some(pay_to) = get("TURNSTILE_X402_PAY_TO") else {
            return Ok(None);
        };
        if !is_evm_address(&pay_to) {
            return Err(TurnstileError::Config(
                "TURNSTILE_X402_PAY_TO must be a 0x-prefixed 20-byte EVM address".into(),
            ));
        }
        let network = get("TURNSTILE_X402_NETWORK").unwrap_or_else(|| DEFAULT_NETWORK.into());
        if !network
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b':')
        {
            return Err(TurnstileError::Config(
                "TURNSTILE_X402_NETWORK must be a network id such as base or base-sepolia".into(),
            ));
        }
        let facilitator_url = match get("TURNSTILE_X402_FACILITATOR_URL") {
            Some(url) => url,
            None if is_testnet(&network) => TESTNET_FACILITATOR.into(),
            None => {
                return Err(TurnstileError::Config(format!(
                    "TURNSTILE_X402_FACILITATOR_URL must be set for network {network} (mainnet settlement needs a facilitator you have an account with, e.g. Coinbase CDP or PayAI)"
                )))
            }
        };
        if !(facilitator_url.starts_with("https://") || facilitator_url.starts_with("http://")) {
            return Err(TurnstileError::Config(
                "TURNSTILE_X402_FACILITATOR_URL must be an http(s) URL".into(),
            ));
        }
        let asset = match get("TURNSTILE_X402_ASSET") {
            Some(asset) => asset,
            None => match network.as_str() {
                "base" | "eip155:8453" => USDC_BASE.into(),
                "base-sepolia" | "eip155:84532" => USDC_BASE_SEPOLIA.into(),
                other => {
                    return Err(TurnstileError::Config(format!(
                        "TURNSTILE_X402_ASSET must be set for network {other} (the USDC contract address)"
                    )))
                }
            },
        };
        if !is_evm_address(&asset) {
            return Err(TurnstileError::Config(
                "TURNSTILE_X402_ASSET must be a 0x-prefixed 20-byte contract address".into(),
            ));
        }
        let max_timeout_seconds = match get("TURNSTILE_X402_MAX_TIMEOUT_SECS") {
            Some(raw) => raw
                .parse::<u64>()
                .ok()
                .filter(|v| (1..=3600).contains(v))
                .ok_or_else(|| {
                    TurnstileError::Config(
                        "TURNSTILE_X402_MAX_TIMEOUT_SECS must be an integer in 1..=3600".into(),
                    )
                })?,
            None => DEFAULT_MAX_TIMEOUT_SECS,
        };
        let public_url = get("TURNSTILE_PUBLIC_URL").map(|u| u.trim_end_matches('/').to_string());
        if let Some(url) = &public_url {
            if !(url.starts_with("https://") || url.starts_with("http://")) {
                return Err(TurnstileError::Config(
                    "TURNSTILE_PUBLIC_URL must be an absolute http(s) origin".into(),
                ));
            }
        }
        Ok(Some(Self {
            pay_to,
            network,
            facilitator_url: facilitator_url.trim_end_matches('/').to_string(),
            facilitator_auth: get("TURNSTILE_X402_FACILITATOR_AUTH"),
            asset,
            asset_name: get("TURNSTILE_X402_ASSET_NAME")
                .unwrap_or_else(|| DEFAULT_ASSET_NAME.into()),
            asset_version: get("TURNSTILE_X402_ASSET_VERSION")
                .unwrap_or_else(|| DEFAULT_ASSET_VERSION.into()),
            max_timeout_seconds,
            mint_account: get("TURNSTILE_X402_MINT_ACCOUNT"),
            public_url,
        }))
    }
}

fn is_testnet(network: &str) -> bool {
    matches!(network, "base-sepolia" | "eip155:84532")
}

/// `0x` followed by exactly 40 hex digits.
#[must_use]
pub fn is_evm_address(value: &str) -> bool {
    value
        .strip_prefix("0x")
        .is_some_and(|hex| hex.len() == 40 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const PAY_TO: &str = "0x1111111111111111111111111111111111111111";

    fn load(pairs: &[(&str, &str)]) -> Result<Option<X402Config>, TurnstileError> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        X402Config::from_lookup(|k| map.get(k).cloned())
    }

    #[test]
    fn disabled_without_pay_to() {
        assert!(load(&[]).unwrap().is_none());
    }

    #[test]
    fn testnet_defaults_to_public_facilitator_and_usdc() {
        let cfg = load(&[("TURNSTILE_X402_PAY_TO", PAY_TO)]).unwrap().unwrap();
        assert_eq!(cfg.network, "base-sepolia");
        assert_eq!(cfg.facilitator_url, TESTNET_FACILITATOR);
        assert_eq!(cfg.asset, USDC_BASE_SEPOLIA);
        assert_eq!(cfg.max_timeout_seconds, 60);
    }

    #[test]
    fn mainnet_requires_an_explicit_facilitator() {
        let err = load(&[
            ("TURNSTILE_X402_PAY_TO", PAY_TO),
            ("TURNSTILE_X402_NETWORK", "base"),
        ])
        .unwrap_err();
        assert!(err.to_string().contains("TURNSTILE_X402_FACILITATOR_URL"));
        let cfg = load(&[
            ("TURNSTILE_X402_PAY_TO", PAY_TO),
            ("TURNSTILE_X402_NETWORK", "base"),
            (
                "TURNSTILE_X402_FACILITATOR_URL",
                "https://facilitator.example/",
            ),
        ])
        .unwrap()
        .unwrap();
        assert_eq!(cfg.asset, USDC_BASE);
        assert_eq!(cfg.facilitator_url, "https://facilitator.example");
    }

    #[test]
    fn rejects_bad_addresses() {
        assert!(load(&[("TURNSTILE_X402_PAY_TO", "ait1qpayee")]).is_err());
        assert!(load(&[
            ("TURNSTILE_X402_PAY_TO", PAY_TO),
            ("TURNSTILE_X402_ASSET", "usdc")
        ])
        .is_err());
    }
}
