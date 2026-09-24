//! USDC on-chain on-ramp. On-ramp checkout + two settlement paths that credit
//! identically:
//!
//! - **On-ramp charge**: [`UsdcRamp::create_charge`] POSTs a fixed-price charge
//!   to the Coinbase Commerce API (`POST /charges`, `X-CC-Api-Key` auth) with
//!   `metadata.account_id`, returning the hosted checkout URL the buyer pays.
//! - **Coinbase Commerce webhook**: `X-CC-Webhook-Signature` = HMAC-SHA256 hex
//!   of the RAW body; event `charge:confirmed`; `asset = Usdc`. On a verified
//!   confirmed charge the handler mints the exact `amount_micros` to `account_id`.
//! - **Re-confirmation of a charge**: `confirm_deposit(<charge id|code>)` GETs
//!   `/charges/{id}` from Commerce and requires a `COMPLETED`/`RESOLVED`
//!   timeline entry; the confirmed amount is the sum of the charge's
//!   CONFIRMED USD payments and the deposit is bound to `metadata.account_id`.
//! - **Pure chain**: a `0x…` tx hash is confirmed via JSON-RPC
//!   (`eth_getTransactionReceipt`), and the amount is DECODED from the ERC-20
//!   `Transfer` logs the USDC token contract emitted to the configured treasury
//!   `deposit_address`.
//!
//! Off-ramp (`initiate_payout`) submits an ERC-20 `transfer(to,amount)` to the
//! USDC token contract via the configured RPC (`eth_sendTransaction`).

use std::sync::Arc;

use async_trait::async_trait;
use mint_core::Micros;
use mint_reserve::{Asset, Deposit, Payout, PayoutDest, PayoutStatus, Ramp, RampError};
use serde_json::Value;

use crate::card::Checkout;
use crate::config::ProviderConfig;
use crate::crypto_tr::micros_to_dollars;
use crate::event::{DepositEvent, WebhookError};
use crate::http::{encode_component, map_status, Headers, HttpTransport};
use crate::money::{decimal_to_micros, json_dollars_to_micros};
use crate::provider::ProviderId;
use crate::verify::hmac_sha256_hex_verify;

/// The Coinbase Commerce REST base (charge creation / hosted checkout).
const COMMERCE_API_BASE: &str = "https://api.commerce.coinbase.com";
/// The Coinbase Commerce API version header value.
const COMMERCE_API_VERSION: &str = "2018-03-22";

/// `keccak256("Transfer(address,address,uint256)")` — the ERC-20 Transfer
/// event topic, a fixed well-known constant.
const ERC20_TRANSFER_TOPIC: &str =
    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

/// The canonical ERC-20 `transfer(address,uint256)` function selector —
/// `keccak256("transfer(address,uint256)")[..4]`, a fixed well-known constant.
const ERC20_TRANSFER_SELECTOR: &str = "a9059cbb";

/// ABI-encode an ERC-20 `transfer(to, amount)` call into the `data` field of an
/// `eth_sendTransaction`: the 4-byte selector, then the 32-byte left-padded
/// recipient address, then the 32-byte big-endian `amount` (in the token's base
/// units). Returns [`RampError::ProviderUnavailable`] for a malformed address
/// (the [`RampError`] taxonomy has no dedicated "bad destination" variant; the
/// redeem burn is already durable, so the payout surfaces as retryable-failed).
fn encode_erc20_transfer(to: &str, amount_base_units: u128) -> Result<String, RampError> {
    let hex_addr = to.strip_prefix("0x").unwrap_or(to);
    if hex_addr.len() != 40 || !hex_addr.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(RampError::ProviderUnavailable);
    }
    // 24 leading zero-nibbles pad the 20-byte (40-hex) address to a 32-byte word;
    // `{:064x}` renders the amount as a 32-byte big-endian word.
    Ok(format!(
        "0x{ERC20_TRANSFER_SELECTOR}{:0>64}{amount_base_units:064x}",
        hex_addr.to_ascii_lowercase()
    ))
}

/// A live USDC [`Ramp`] over an injected [`HttpTransport`] (JSON-RPC node).
pub struct UsdcRamp {
    transport: Arc<dyn HttpTransport>,
    cfg: ProviderConfig,
}

impl UsdcRamp {
    /// Build the adapter from its transport + resolved config.
    pub fn new(transport: Arc<dyn HttpTransport>, cfg: ProviderConfig) -> Self {
        UsdcRamp { transport, cfg }
    }

    fn rpc_url(&self) -> Result<String, RampError> {
        self.cfg
            .extra("rpc_url")
            .filter(|u| !u.is_empty())
            .map(str::to_string)
            .ok_or(RampError::ProviderUnavailable)
    }

    fn commerce_headers(&self) -> Result<Vec<(String, String)>, RampError> {
        let api_key = self
            .cfg
            .extra("commerce_api_key")
            .filter(|k| !k.is_empty())
            .ok_or(RampError::ProviderUnavailable)?;
        Ok(vec![
            ("x-cc-api-key".to_string(), api_key.to_string()),
            ("x-cc-version".to_string(), COMMERCE_API_VERSION.to_string()),
        ])
    }

    /// Re-confirm a Coinbase Commerce charge by id or code.
    async fn confirm_charge(&self, charge_ref: &str) -> Result<Deposit, RampError> {
        let url = format!("{COMMERCE_API_BASE}/charges/{}", encode_component(charge_ref));
        let resp = map_status(self.transport.get(&url, &self.commerce_headers()?).await)?;
        let v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        let data = &v["data"];
        if !data.is_object() {
            return Err(RampError::DepositNotFound);
        }
        let completed = data["timeline"].as_array().is_some_and(|timeline| {
            timeline.iter().any(|entry| {
                matches!(
                    entry["status"].as_str(),
                    Some("COMPLETED") | Some("CONFIRMED") | Some("RESOLVED")
                )
            })
        }) || data["confirmed_at"].as_str().is_some_and(|s| !s.is_empty());
        if !completed {
            return Err(RampError::NotConfirmed);
        }
        let amount = confirmed_payments_usd(data)
            .or_else(|| json_dollars_to_micros(&data["pricing"]["local"]["amount"]))
            .filter(|&m| m > 0)
            .ok_or(RampError::NotConfirmed)?;
        let bound_account = data["metadata"]["account_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            // A Mint charge ALWAYS carries its account; a charge without one
            // was not created by Mint and must not be credited.
            .ok_or(RampError::NotConfirmed)?;
        Ok(Deposit {
            deposit_ref: charge_ref.to_string(),
            amount_micros: Micros::new(amount),
            asset: Asset::Usdc,
            confirmed: true,
            bound_account: Some(bound_account),
        })
    }

    /// Confirm a raw on-chain deposit tx and decode how much USDC it moved to
    /// the treasury `deposit_address`.
    async fn confirm_tx(&self, tx_hash: &str) -> Result<Deposit, RampError> {
        let token = self
            .cfg
            .extra("token_contract")
            .filter(|c| !c.is_empty())
            .ok_or(RampError::ProviderUnavailable)?
            .to_ascii_lowercase();
        let treasury = self
            .cfg
            .extra("deposit_address")
            .filter(|a| !a.is_empty())
            .ok_or(RampError::ProviderUnavailable)?
            .to_ascii_lowercase();
        let v = self
            .rpc_call("eth_getTransactionReceipt", serde_json::json!([tx_hash]))
            .await?;
        let result = &v["result"];
        if result.is_null() {
            return Err(RampError::DepositNotFound);
        }
        if result["status"].as_str() != Some("0x1") {
            return Err(RampError::NotConfirmed);
        }
        let amount = decode_transfers_to(result, &token, &treasury)
            .filter(|&m| m > 0)
            .ok_or(RampError::NotConfirmed)?;
        Ok(Deposit {
            deposit_ref: tx_hash.to_string(),
            amount_micros: Micros::new(amount),
            asset: Asset::Usdc,
            confirmed: true,
            bound_account: None,
        })
    }

    /// Create a hosted Coinbase Commerce charge crediting `account_id` for
    /// `amount_micros` (on-ramp). The `metadata.account_id` is echoed back on the
    /// later `charge:confirmed` webhook so the mint target resolves. Returns the
    /// hosted `checkout_url` (`data.hosted_url`) + charge id (`data.code`).
    ///
    /// Env-key gated: the Coinbase Commerce API key lives in
    /// `extra("commerce_api_key")` (deploy-populated). When absent the on-ramp
    /// charge path is inactive and returns [`RampError::ProviderUnavailable`]
    /// WITHOUT any network call — the pure-chain watcher path stays available.
    pub async fn create_charge(
        &self,
        account_id: &str,
        amount_micros: i64,
    ) -> Result<Checkout, RampError> {
        let api_key = self
            .cfg
            .extra("commerce_api_key")
            .filter(|k| !k.is_empty())
            .ok_or(RampError::ProviderUnavailable)?;
        let body = serde_json::json!({
            "name": "AIT deposit",
            "description": "USDC on-ramp deposit to Mint",
            "pricing_type": "fixed_price",
            "local_price": {
                "amount": micros_to_dollars(amount_micros),
                "currency": "USD",
            },
            "metadata": { "account_id": account_id },
        });
        let raw = serde_json::to_vec(&body).expect("json");
        let headers = vec![
            ("x-cc-api-key".to_string(), api_key.to_string()),
            ("x-cc-version".to_string(), COMMERCE_API_VERSION.to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ];
        let url = format!("{COMMERCE_API_BASE}/charges");
        let resp = map_status(self.transport.post_json(&url, &headers, &raw).await)?;
        let v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        let data = &v["data"];
        let checkout_url = data["hosted_url"]
            .as_str()
            .ok_or(RampError::ProviderUnavailable)?
            .to_string();
        let checkout_id = data["code"]
            .as_str()
            .or_else(|| data["id"].as_str())
            .ok_or(RampError::ProviderUnavailable)?
            .to_string();
        Ok(Checkout {
            checkout_url,
            checkout_id,
        })
    }

    async fn rpc_call(&self, method: &str, params: Value) -> Result<Value, RampError> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": method,
            "params": params,
        });
        let headers = vec![("content-type".to_string(), "application/json".to_string())];
        let raw = serde_json::to_vec(&body).expect("json");
        let resp = map_status(
            self.transport
                .post_json(&self.rpc_url()?, &headers, &raw)
                .await,
        )?;
        serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)
    }
}

#[async_trait]
impl Ramp for UsdcRamp {
    async fn confirm_deposit(&self, deposit_ref: &str) -> Result<Deposit, RampError> {
        // Two deposit reference kinds: a 32-byte `0x…` tx hash (pure chain) or
        // a Coinbase Commerce charge id/code (hosted on-ramp, what the
        // `charge:confirmed` webhook keys on).
        if is_tx_hash(deposit_ref) {
            self.confirm_tx(deposit_ref).await
        } else {
            self.confirm_charge(deposit_ref).await
        }
    }

    async fn initiate_payout(
        &self,
        payout_ref: &str,
        dest: &PayoutDest,
        amount: Micros,
    ) -> Result<Payout, RampError> {
        // USDC pays out as an ERC-20 `transfer(to, amount)` to the token
        // contract — NOT a native-value transfer. Signing is performed by the
        // configured node / relayer (`eth_sendTransaction`); an optional
        // `payout_from` selects the operator hot wallet when the node holds many.
        let address = match dest {
            PayoutDest::Crypto { address, .. } => address.as_str(),
            _ => return Err(RampError::ProviderUnavailable),
        };
        // USDC is a 6-decimal ERC-20 and microdollars are also 6-decimal, so the
        // payout base-unit amount equals the microdollar amount 1:1
        // (1 µ$ = 1 USDC base unit). A non-positive amount can never reach here
        // (redeem enforces strictly-positive) but is rejected defensively.
        let amount_base_units =
            u128::try_from(amount.get()).map_err(|_| RampError::ProviderUnavailable)?;
        let token_contract = self
            .cfg
            .extra("token_contract")
            .filter(|c| !c.is_empty())
            .ok_or(RampError::ProviderUnavailable)?
            .to_string();
        let data = encode_erc20_transfer(address, amount_base_units)?;
        let mut tx = serde_json::json!({
            "to": token_contract,
            "value": "0x0",
            "data": data,
        });
        if let Some(from) = self.cfg.extra("payout_from").filter(|f| !f.is_empty()) {
            tx["from"] = serde_json::json!(from);
        }
        let sent = self
            .rpc_call("eth_sendTransaction", serde_json::json!([tx]))
            .await?;
        if sent["error"].is_object() {
            return Err(RampError::ProviderUnavailable);
        }
        let tx_hash = sent["result"]
            .as_str()
            .filter(|h| !h.is_empty())
            .ok_or(RampError::ProviderUnavailable)?
            .to_string();
        // Submitted, not yet mined: settlement is tracked via `payout_status`
        // (the transaction receipt) until the transfer is final.
        Ok(Payout {
            payout_ref: payout_ref.to_string(),
            amount_micros: amount,
            status: PayoutStatus::Pending,
            provider_ref: Some(tx_hash),
        })
    }

    async fn check_payout(
        &self,
        _payout_ref: &str,
        dest: &PayoutDest,
        amount: Micros,
    ) -> Result<(), RampError> {
        let PayoutDest::Crypto { address, .. } = dest else {
            return Err(RampError::Unsupported);
        };
        if self
            .cfg
            .extra("token_contract")
            .filter(|c| !c.is_empty())
            .is_none()
            || self.rpc_url().is_err()
        {
            // Payouts need both the token contract and a signing JSON-RPC node.
            return Err(RampError::Unsupported);
        }
        let amount_base_units = u128::try_from(amount.get()).map_err(|_| RampError::Unsupported)?;
        encode_erc20_transfer(address, amount_base_units)
            .map(|_| ())
            .map_err(|_| RampError::Unsupported)
    }

    async fn payout_status(
        &self,
        _payout_ref: &str,
        provider_ref: Option<&str>,
    ) -> Result<PayoutStatus, RampError> {
        let tx_hash = provider_ref
            .filter(|h| !h.is_empty())
            .ok_or(RampError::Unsupported)?;
        let v = self
            .rpc_call("eth_getTransactionReceipt", serde_json::json!([tx_hash]))
            .await?;
        let result = &v["result"];
        if result.is_null() {
            return Ok(PayoutStatus::Pending);
        }
        Ok(match result["status"].as_str() {
            Some("0x1") => PayoutStatus::Paid,
            Some("0x0") => PayoutStatus::Failed,
            _ => PayoutStatus::Pending,
        })
    }
}

/// `true` for a `0x`-prefixed 32-byte hex transaction hash.
fn is_tx_hash(s: &str) -> bool {
    s.strip_prefix("0x")
        .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Sum the charge's CONFIRMED payments in USD micros. `None` when the charge
/// reports no confirmed USD payment (the caller then falls back to the fixed
/// price, which a `COMPLETED` timeline guarantees was paid in full).
fn confirmed_payments_usd(data: &Value) -> Option<i64> {
    let payments = data["payments"].as_array()?;
    let mut total: i64 = 0;
    let mut any = false;
    for p in payments {
        let status = p["status"].as_str().unwrap_or("");
        if !status.eq_ignore_ascii_case("CONFIRMED") {
            continue;
        }
        let local = &p["value"]["local"];
        if !local["currency"]
            .as_str()
            .is_some_and(|c| c.eq_ignore_ascii_case("USD"))
        {
            continue;
        }
        total = total.checked_add(json_dollars_to_micros(&local["amount"])?)?;
        any = true;
    }
    any.then_some(total)
}

/// Sum the ERC-20 `Transfer(from, to, value)` logs emitted by `token` whose
/// recipient is `treasury`. USDC has 6 decimals, so base units == µ$.
fn decode_transfers_to(receipt: &Value, token: &str, treasury: &str) -> Option<i64> {
    let treasury_hex = treasury.strip_prefix("0x").unwrap_or(treasury);
    let mut total: i64 = 0;
    for log in receipt["logs"].as_array()? {
        let address = log["address"].as_str().unwrap_or("").to_ascii_lowercase();
        if address != token {
            continue;
        }
        let topics = match log["topics"].as_array() {
            Some(t) if t.len() == 3 => t,
            _ => continue,
        };
        if !topics[0]
            .as_str()
            .is_some_and(|t| t.eq_ignore_ascii_case(ERC20_TRANSFER_TOPIC))
        {
            continue;
        }
        let to_word = topics[2].as_str().unwrap_or("").to_ascii_lowercase();
        if !to_word.ends_with(treasury_hex) {
            continue;
        }
        let data = log["data"].as_str().unwrap_or("");
        let hex = data.strip_prefix("0x").unwrap_or(data).trim_start_matches('0');
        let value = if hex.is_empty() {
            0
        } else if hex.len() > 16 {
            return None; // larger than any i64 amount: refuse rather than wrap
        } else {
            i64::try_from(u64::from_str_radix(hex, 16).ok()?).ok()?
        };
        total = total.checked_add(value)?;
    }
    Some(total)
}

/// Verify a Coinbase Commerce webhook over RAW bytes.
pub fn verify_webhook<H: Headers>(
    cfg: &ProviderConfig,
    headers: &H,
    raw: &[u8],
) -> Result<DepositEvent, WebhookError> {
    let secret = cfg
        .webhook_secret
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or(WebhookError::NotConfigured)?;
    let sig = headers
        .get_header("x-cc-webhook-signature")
        .ok_or(WebhookError::BadSignature)?;
    hmac_sha256_hex_verify(secret.as_bytes(), raw, sig)?;

    let v: Value =
        serde_json::from_slice(raw).map_err(|e| WebhookError::Malformed(e.to_string()))?;
    let event = &v["event"];
    let event_type = event["type"].as_str().unwrap_or("");
    if event_type != "charge:confirmed" {
        return Err(WebhookError::Ignored(event_type.to_string()));
    }
    let data = &event["data"];
    let deposit_ref = data["id"]
        .as_str()
        .or_else(|| data["code"].as_str())
        .ok_or_else(|| WebhookError::Malformed("missing charge id".into()))?
        .to_string();
    let amount_micros = json_dollars_to_micros(&data["pricing"]["local"]["amount"])
        .or_else(|| {
            data["pricing"]["local"]["amount"]
                .as_str()
                .and_then(decimal_to_micros)
        })
        .ok_or_else(|| WebhookError::Malformed("missing pricing.local.amount".into()))?;
    let account_id = data["metadata"]["account_id"]
        .as_str()
        .ok_or_else(|| WebhookError::Malformed("missing metadata.account_id".into()))?
        .to_string();
    Ok(DepositEvent {
        provider: ProviderId::Usdc,
        deposit_ref,
        amount_micros,
        asset: Asset::Usdc,
        account_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Mode;
    use crate::crypto_tr::test_support::{headers, hmac_hex, FakeTransport};
    use std::collections::HashMap;

    /// The mainnet USDC token contract — used to assert payouts target the token,
    /// not the recipient EOA.
    const USDC_TOKEN: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";

    fn cfg(secret: Option<&str>, commerce_key: Option<&str>) -> ProviderConfig {
        let mut extra = HashMap::new();
        extra.insert("rpc_url".to_string(), "https://rpc.local".to_string());
        extra.insert("token_contract".to_string(), USDC_TOKEN.to_string());
        if let Some(k) = commerce_key {
            extra.insert("commerce_api_key".to_string(), k.to_string());
        }
        ProviderConfig {
            provider: ProviderId::Usdc,
            api_key: Some("https://rpc.local".into()),
            webhook_secret: secret.map(|s| s.to_string()),
            mode: Mode::Sandbox,
            extra,
        }
    }

    // ---- on-ramp: real Coinbase Commerce charge creation --------------------

    #[tokio::test]
    async fn create_charge_posts_commerce_charge_and_returns_hosted_url() {
        let transport = FakeTransport::new()
            .route(
                "commerce.coinbase.com/charges",
                201,
                serde_json::json!({"data":{"id":"uuid-1","code":"CHRG42","hosted_url":"https://commerce.coinbase.com/charges/CHRG42"}}),
            )
            .arc();
        let ramp = UsdcRamp::new(transport.clone(), cfg(Some("s"), Some("cc_live_key")));
        let checkout = ramp
            .create_charge("acct_x", 25_000_000)
            .await
            .expect("charge created");
        assert_eq!(checkout.checkout_id, "CHRG42");
        assert_eq!(
            checkout.checkout_url,
            "https://commerce.coinbase.com/charges/CHRG42"
        );
        // The request body carries the exact price + account metadata.
        let body = String::from_utf8(transport.last_post_body()).unwrap();
        assert!(body.contains("\"amount\":\"25.00\""), "body: {body}");
        assert!(body.contains("\"account_id\":\"acct_x\""), "body: {body}");
    }

    #[tokio::test]
    async fn create_charge_without_commerce_key_is_inactive() {
        // No commerce_api_key configured ⇒ on-ramp charge path inactive, no call.
        let transport = FakeTransport::new().arc();
        let ramp = UsdcRamp::new(transport, cfg(Some("s"), None));
        assert_eq!(
            ramp.create_charge("acct_x", 1_000_000).await,
            Err(RampError::ProviderUnavailable)
        );
    }

    // ---- webhook: valid signed → exact mint amount; tampered → rejected -----

    #[test]
    fn valid_signed_charge_confirmed_yields_exact_amount() {
        let secret = "cc-shared-secret";
        let body = br#"{"event":{"type":"charge:confirmed","data":{"code":"CHRG42","pricing":{"local":{"amount":"25.00"}},"metadata":{"account_id":"acct_x"}}}}"#;
        let hdrs = headers(&[("x-cc-webhook-signature", &hmac_hex(secret.as_bytes(), body))]);
        let ev = verify_webhook(&cfg(Some(secret), None), &hdrs, body).expect("verifies");
        assert_eq!(ev.deposit_ref, "CHRG42");
        assert_eq!(ev.amount_micros, 25_000_000); // exact AIT credited
        assert_eq!(ev.asset, Asset::Usdc);
        assert_eq!(ev.account_id, "acct_x");
        assert_eq!(ev.namespaced_ref(), "usdc:CHRG42");
    }

    #[test]
    fn tampered_signature_is_rejected() {
        let secret = "cc-shared-secret";
        let body = br#"{"event":{"type":"charge:confirmed","data":{"id":"c","pricing":{"local":{"amount":"1.00"}},"metadata":{"account_id":"a"}}}}"#;
        let mut sig = hmac_hex(secret.as_bytes(), body);
        sig.pop();
        sig.push(if sig.ends_with('a') { 'b' } else { 'a' });
        let hdrs = headers(&[("x-cc-webhook-signature", &sig)]);
        assert_eq!(
            verify_webhook(&cfg(Some(secret), None), &hdrs, body),
            Err(WebhookError::BadSignature)
        );
    }

    #[test]
    fn tampered_body_under_valid_old_signature_is_rejected() {
        let secret = "cc-shared-secret";
        let signed = br#"{"event":{"type":"charge:confirmed","data":{"code":"CHRG42","pricing":{"local":{"amount":"25.00"}},"metadata":{"account_id":"acct_x"}}}}"#;
        let sig = hmac_hex(secret.as_bytes(), signed);
        // Attacker inflates the amount but reuses the signature over the old body.
        let tampered = br#"{"event":{"type":"charge:confirmed","data":{"code":"CHRG42","pricing":{"local":{"amount":"9999.00"}},"metadata":{"account_id":"acct_x"}}}}"#;
        let hdrs = headers(&[("x-cc-webhook-signature", &sig)]);
        assert_eq!(
            verify_webhook(&cfg(Some(secret), None), &hdrs, tampered),
            Err(WebhookError::BadSignature)
        );
    }

    #[test]
    fn unconfigured_webhook_secret_is_not_configured() {
        let body = br#"{"event":{"type":"charge:confirmed"}}"#;
        let hdrs = headers(&[("x-cc-webhook-signature", "00")]);
        assert_eq!(
            verify_webhook(&cfg(None, None), &hdrs, body),
            Err(WebhookError::NotConfigured)
        );
    }

    #[test]
    fn non_confirmed_event_is_ignored() {
        let secret = "cc-shared-secret";
        let body = br#"{"event":{"type":"charge:pending","data":{"code":"c"}}}"#;
        let hdrs = headers(&[("x-cc-webhook-signature", &hmac_hex(secret.as_bytes(), body))]);
        assert_eq!(
            verify_webhook(&cfg(Some(secret), None), &hdrs, body),
            Err(WebhookError::Ignored("charge:pending".into()))
        );
    }

    // ---- on-chain confirm + off-ramp payout ---------------------------------

    const TREASURY: &str = "0x2222222222222222222222222222222222222222";
    const TX: &str = "0x00000000000000000000000000000000000000000000000000000000000000ab";

    fn chain_cfg() -> ProviderConfig {
        let mut c = cfg(Some("s"), None);
        c.extra.insert("deposit_address".into(), TREASURY.into());
        c
    }

    #[tokio::test]
    async fn confirm_tx_decodes_transfer_log_to_treasury() {
        let receipt = serde_json::json!({"result":{"status":"0x1","logs":[{
            "address": USDC_TOKEN.to_ascii_lowercase(),
            "topics": [
                ERC20_TRANSFER_TOPIC,
                "0x0000000000000000000000001111111111111111111111111111111111111111",
                "0x0000000000000000000000002222222222222222222222222222222222222222"
            ],
            "data": format!("0x{:064x}", 25_000_000u64)
        }]}});
        let ok = FakeTransport::new().route("rpc.local", 200, receipt).arc();
        let dep = UsdcRamp::new(ok, chain_cfg()).confirm_deposit(TX).await.expect("confirmed");
        assert!(dep.confirmed);
        assert_eq!(dep.amount_micros, Micros::new(25_000_000));
        assert_eq!(dep.asset, Asset::Usdc);

        let nf = FakeTransport::new()
            .route("rpc.local", 200, serde_json::json!({"result": null}))
            .arc();
        assert_eq!(
            UsdcRamp::new(nf, chain_cfg()).confirm_deposit(TX).await,
            Err(RampError::DepositNotFound)
        );

        // A successful tx that moved nothing to the treasury never mints.
        let other = FakeTransport::new()
            .route("rpc.local", 200, serde_json::json!({"result":{"status":"0x1","logs":[]}}))
            .arc();
        assert_eq!(
            UsdcRamp::new(other, chain_cfg()).confirm_deposit(TX).await,
            Err(RampError::NotConfirmed)
        );
    }

    #[tokio::test]
    async fn confirm_charge_reads_commerce_payments_and_binds_account() {
        let charge = serde_json::json!({"data":{
            "code":"CHRG42",
            "timeline":[{"status":"NEW"},{"status":"PENDING"},{"status":"COMPLETED"}],
            "pricing":{"local":{"amount":"25.00","currency":"USD"}},
            "payments":[{"status":"CONFIRMED","value":{"local":{"amount":"25.00","currency":"USD"}}}],
            "metadata":{"account_id":"acct_x"}
        }});
        let t = FakeTransport::new().route("/charges/CHRG42", 200, charge).arc();
        let dep = UsdcRamp::new(t, cfg(Some("s"), Some("cc_key")))
            .confirm_deposit("CHRG42")
            .await
            .expect("confirmed");
        assert_eq!(dep.amount_micros, Micros::new(25_000_000));
        assert_eq!(dep.bound_account.as_deref(), Some("acct_x"));

        let pending = serde_json::json!({"data":{"code":"C2","timeline":[{"status":"NEW"}],"metadata":{"account_id":"a"}}});
        let t = FakeTransport::new().route("/charges/C2", 200, pending).arc();
        assert_eq!(
            UsdcRamp::new(t, cfg(Some("s"), Some("cc_key"))).confirm_deposit("C2").await,
            Err(RampError::NotConfirmed)
        );
    }

    #[tokio::test]
    async fn payout_to_stripe_dest_is_unsupported() {
        let ramp = UsdcRamp::new(FakeTransport::new().arc(), cfg(Some("s"), None));
        let dest = PayoutDest::Stripe {
            account: "acct".into(),
        };
        assert_eq!(
            ramp.initiate_payout("usdc:p1", &dest, Micros::new(1_000_000))
                .await,
            Err(RampError::ProviderUnavailable)
        );
    }

    #[tokio::test]
    async fn payout_encodes_erc20_transfer_to_token_contract() {
        let transport = FakeTransport::new()
            .route("rpc.local", 200, serde_json::json!({"result":"0xtxhash"}))
            .arc();
        let ramp = UsdcRamp::new(transport.clone(), cfg(Some("s"), None));
        let dest = PayoutDest::Crypto {
            chain: "ethereum".into(),
            address: "0x1111111111111111111111111111111111111111".into(),
        };
        let payout = ramp
            .initiate_payout("usdc:p1", &dest, Micros::new(25_000_000))
            .await
            .expect("payout submitted");
        // Submitted on-chain but not yet mined ⇒ Pending, tracked by tx hash.
        assert_eq!(payout.status, PayoutStatus::Pending);
        assert_eq!(payout.provider_ref.as_deref(), Some("0xtxhash"));
        assert_eq!(payout.amount_micros, Micros::new(25_000_000));

        let body = String::from_utf8(transport.last_post_body()).unwrap();
        // The tx targets the USDC token contract, carries ZERO native value, and
        // moves funds via `transfer(to,amount)` calldata — NOT a native transfer
        // of `amount` wei (the prior bug).
        assert!(body.contains(&format!("\"to\":\"{USDC_TOKEN}\"")), "body: {body}");
        assert!(body.contains("\"value\":\"0x0\""), "body: {body}");
        assert!(!body.contains("25000000"), "must not send raw micros as wei: {body}");
        // selector + 32-byte padded recipient + 32-byte big-endian amount
        // (25_000_000 µ$ == 25_000_000 USDC base units == 0x17d7840).
        let expected_data = format!(
            "0xa9059cbb{:0>64}{:064x}",
            "1111111111111111111111111111111111111111", 25_000_000u128
        );
        assert!(body.contains(&expected_data), "body: {body}");
    }

    #[tokio::test]
    async fn payout_without_token_contract_is_unavailable() {
        let mut cfg = cfg(Some("s"), None);
        cfg.extra.remove("token_contract");
        let ramp = UsdcRamp::new(FakeTransport::new().arc(), cfg);
        let dest = PayoutDest::Crypto {
            chain: "ethereum".into(),
            address: "0x1111111111111111111111111111111111111111".into(),
        };
        assert_eq!(
            ramp.initiate_payout("usdc:p1", &dest, Micros::new(1_000_000))
                .await,
            Err(RampError::ProviderUnavailable)
        );
    }

    #[tokio::test]
    async fn payout_to_malformed_address_is_rejected() {
        let ramp = UsdcRamp::new(FakeTransport::new().arc(), cfg(Some("s"), None));
        let dest = PayoutDest::Crypto {
            chain: "ethereum".into(),
            address: "0xnothex".into(),
        };
        assert_eq!(
            ramp.initiate_payout("usdc:p1", &dest, Micros::new(1_000_000))
                .await,
            Err(RampError::ProviderUnavailable)
        );
    }
}
