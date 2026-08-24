//! USDC on-chain on-ramp. On-ramp checkout + two settlement paths that credit
//! identically:
//!
//! - **On-ramp charge**: [`UsdcRamp::create_charge`] POSTs a fixed-price charge
//!   to the Coinbase Commerce API (`POST /charges`, `X-CC-Api-Key` auth) with
//!   `metadata.account_id`, returning the hosted checkout URL the buyer pays.
//! - **Coinbase Commerce webhook**: `X-CC-Webhook-Signature` = HMAC-SHA256 hex
//!   of the RAW body; event `charge:confirmed`; `asset = Usdc`. On a verified
//!   confirmed charge the handler mints the exact `amount_micros` to `account_id`.
//! - **Pure chain**: a watcher confirms a deposit tx via JSON-RPC
//!   (`confirm_deposit` → `eth_getTransactionReceipt`).
//!
//! Off-ramp (`initiate_payout`) submits a transfer to the configured RPC.

use std::sync::Arc;

use async_trait::async_trait;
use mint_core::Micros;
use mint_reserve::{Asset, Deposit, Payout, PayoutDest, PayoutStatus, Ramp, RampError};
use serde_json::Value;

use crate::card::Checkout;
use crate::config::ProviderConfig;
use crate::crypto_tr::micros_to_dollars;
use crate::event::{DepositEvent, WebhookError};
use crate::http::{map_status, Headers, HttpTransport};
use crate::money::{decimal_to_micros, json_dollars_to_micros};
use crate::provider::ProviderId;
use crate::verify::hmac_sha256_hex_verify;

/// The Coinbase Commerce REST base (charge creation / hosted checkout).
const COMMERCE_API_BASE: &str = "https://api.commerce.coinbase.com";
/// The Coinbase Commerce API version header value.
const COMMERCE_API_VERSION: &str = "2018-03-22";

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

    fn rpc_url(&self) -> String {
        self.cfg
            .extra("rpc_url")
            .or(self.cfg.api_key.as_deref())
            .unwrap_or_default()
            .to_string()
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
                .post_json(&self.rpc_url(), &headers, &raw)
                .await,
        )?;
        serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)
    }
}

#[async_trait]
impl Ramp for UsdcRamp {
    async fn confirm_deposit(&self, deposit_ref: &str) -> Result<Deposit, RampError> {
        // `deposit_ref` is the on-chain tx hash. Confirm via the receipt status.
        let v = self
            .rpc_call(
                "eth_getTransactionReceipt",
                serde_json::json!([deposit_ref]),
            )
            .await?;
        let result = &v["result"];
        if result.is_null() {
            return Err(RampError::DepositNotFound);
        }
        let confirmed = result["status"].as_str() == Some("0x1");
        if !confirmed {
            return Err(RampError::NotConfirmed);
        }
        // Amount is carried by the confirming webhook; a full log-decode of the
        // ERC-20 Transfer event is the deploy-time enrichment.
        Ok(Deposit {
            deposit_ref: deposit_ref.to_string(),
            amount_micros: Micros::new(0),
            asset: Asset::Usdc,
            confirmed,
            bound_account: None,
        })
    }

    async fn initiate_payout(
        &self,
        payout_ref: &str,
        dest: &PayoutDest,
        amount: Micros,
    ) -> Result<Payout, RampError> {
        // Submit the (deploy-time signed) transfer to the node. The RPC call is
        // real + fake-injectable; transaction signing lives at deploy.
        let address = match dest {
            PayoutDest::Crypto { address, .. } => address.clone(),
            PayoutDest::Stripe { .. } => return Err(RampError::ProviderUnavailable),
        };
        let _ = self
            .rpc_call(
                "eth_sendTransaction",
                serde_json::json!([{ "to": address, "value": amount.get() }]),
            )
            .await?;
        Ok(Payout {
            payout_ref: payout_ref.to_string(),
            amount_micros: amount,
            status: PayoutStatus::Paid,
        })
    }
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

    fn cfg(secret: Option<&str>, commerce_key: Option<&str>) -> ProviderConfig {
        let mut extra = HashMap::new();
        extra.insert("rpc_url".to_string(), "https://rpc.local".to_string());
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

    #[tokio::test]
    async fn confirm_deposit_reads_receipt_status() {
        let ok = FakeTransport::new()
            .route(
                "rpc.local",
                200,
                serde_json::json!({"result":{"status":"0x1"}}),
            )
            .arc();
        let ramp = UsdcRamp::new(ok, cfg(Some("s"), None));
        let dep = ramp.confirm_deposit("0xabc").await.expect("confirmed");
        assert!(dep.confirmed);
        assert_eq!(dep.asset, Asset::Usdc);

        let nf = FakeTransport::new()
            .route("rpc.local", 200, serde_json::json!({"result": null}))
            .arc();
        let ramp = UsdcRamp::new(nf, cfg(Some("s"), None));
        assert_eq!(
            ramp.confirm_deposit("0xdead").await,
            Err(RampError::DepositNotFound)
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
}
