//! Creem adapter (merchant-of-record, billing-only). On-ramp: checkout create +
//! `checkout` re-confirmation. NO arbitrary-payout API ⇒ `initiate_payout`
//! returns `ProviderUnavailable`. Webhook: `creem-signature` = HMAC-SHA256 hex
//! of the RAW body; events `checkout.completed` / `subscription.paid`.

use std::sync::Arc;

use async_trait::async_trait;
use mint_core::Micros;
use mint_reserve::{Asset, Deposit, Payout, PayoutDest, Ramp, RampError};
use serde_json::Value;

use crate::card::Checkout;
use crate::config::ProviderConfig;
use crate::event::{DepositEvent, WebhookError};
use crate::http::{map_status, Headers, HttpTransport};
use crate::money::cents_to_micros;
use crate::provider::ProviderId;
use crate::verify::hmac_sha256_hex_verify;

const API_BASE: &str = "https://api.creem.io";

/// A live Creem [`Ramp`] over an injected [`HttpTransport`].
pub struct CreemRamp {
    transport: Arc<dyn HttpTransport>,
    cfg: ProviderConfig,
}

impl CreemRamp {
    /// Build the adapter from its transport + resolved config.
    pub fn new(transport: Arc<dyn HttpTransport>, cfg: ProviderConfig) -> Self {
        CreemRamp { transport, cfg }
    }

    fn auth_headers(&self) -> Vec<(String, String)> {
        vec![(
            "x-api-key".into(),
            self.cfg.api_key.clone().unwrap_or_default(),
        )]
    }

    /// Create a Creem checkout crediting `account_id`, threading the account id
    /// through `metadata` for the webhook to resolve.
    pub async fn create_checkout(
        &self,
        account_id: &str,
        amount_micros: i64,
    ) -> Result<Checkout, RampError> {
        let body = serde_json::json!({
            "units": 1,
            "amount": amount_micros / 10_000,
            "currency": "USD",
            "metadata": { "account_id": account_id },
        });
        let mut headers = self.auth_headers();
        headers.push(("content-type".into(), "application/json".into()));
        let url = format!("{API_BASE}/v1/checkouts");
        let raw = serde_json::to_vec(&body).expect("json");
        let resp = map_status(self.transport.post_json(&url, &headers, &raw).await)?;
        let v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        Ok(Checkout {
            checkout_url: v["checkout_url"].as_str().unwrap_or_default().to_string(),
            checkout_id: v["id"].as_str().unwrap_or_default().to_string(),
        })
    }
}

#[async_trait]
impl Ramp for CreemRamp {
    async fn confirm_deposit(&self, deposit_ref: &str) -> Result<Deposit, RampError> {
        let url = format!("{API_BASE}/v1/checkouts/{deposit_ref}");
        let resp = map_status(self.transport.get(&url, &self.auth_headers()).await)?;
        let v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        let status = v["status"].as_str().unwrap_or("");
        let confirmed = matches!(status, "completed" | "paid");
        if !confirmed {
            return Err(RampError::NotConfirmed);
        }
        let cents = order_amount_cents(&v).ok_or(RampError::ProviderUnavailable)?;
        let micros = cents_to_micros(cents).ok_or(RampError::ProviderUnavailable)?;
        Ok(Deposit {
            deposit_ref: deposit_ref.to_string(),
            amount_micros: Micros::new(micros),
            asset: Asset::Usd,
            confirmed,
            bound_account: None,
        })
    }

    async fn initiate_payout(
        &self,
        _payout_ref: &str,
        _dest: &PayoutDest,
        _amount: Micros,
    ) -> Result<Payout, RampError> {
        // Creem is billing-only: there is no arbitrary-payout API.
        Err(RampError::ProviderUnavailable)
    }
}

/// Verify a Creem webhook over RAW bytes.
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
        .get_header("creem-signature")
        .ok_or(WebhookError::BadSignature)?;
    hmac_sha256_hex_verify(secret.as_bytes(), raw, sig)?;

    let v: Value =
        serde_json::from_slice(raw).map_err(|e| WebhookError::Malformed(e.to_string()))?;
    let event_type = v["eventType"]
        .as_str()
        .or_else(|| v["type"].as_str())
        .unwrap_or("");
    if !matches!(event_type, "checkout.completed" | "subscription.paid") {
        return Err(WebhookError::Ignored(event_type.to_string()));
    }
    let obj = &v["object"];
    let deposit_ref = obj["id"]
        .as_str()
        .ok_or_else(|| WebhookError::Malformed("missing object.id".into()))?
        .to_string();
    let cents =
        order_amount_cents(obj).ok_or_else(|| WebhookError::Malformed("missing amount".into()))?;
    let amount_micros =
        cents_to_micros(cents).ok_or_else(|| WebhookError::Malformed("amount overflow".into()))?;
    let account_id = obj["metadata"]["account_id"]
        .as_str()
        .or_else(|| obj["request_id"].as_str())
        .ok_or_else(|| WebhookError::Malformed("missing metadata.account_id".into()))?
        .to_string();
    Ok(DepositEvent {
        provider: ProviderId::Creem,
        deposit_ref,
        amount_micros,
        asset: Asset::Usd,
        account_id,
    })
}

/// Creem reports the paid amount either at `object.order.amount` or
/// `object.amount` (minor units).
fn order_amount_cents(obj: &Value) -> Option<i64> {
    obj["order"]["amount"]
        .as_i64()
        .or_else(|| obj["amount"].as_i64())
}
