//! Polar adapter (merchant-of-record, billing-only). On-ramp: checkout create +
//! order re-confirmation. NO arbitrary-payout API ⇒ `initiate_payout` returns
//! `ProviderUnavailable`. Webhook: Standard Webhooks headers
//! `webhook-id`/`webhook-timestamp`/`webhook-signature` (base64 HMAC-SHA256 over
//! `"{id}.{ts}.{body}"`, secret base64 after the `whsec_` strip); events
//! `order.paid` / `checkout.updated` (succeeded).

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
use crate::verify::standard_webhooks_verify;

const API_BASE: &str = "https://api.polar.sh";

/// A live Polar [`Ramp`] over an injected [`HttpTransport`].
pub struct PolarRamp {
    transport: Arc<dyn HttpTransport>,
    cfg: ProviderConfig,
}

impl PolarRamp {
    /// Build the adapter from its transport + resolved config.
    pub fn new(transport: Arc<dyn HttpTransport>, cfg: ProviderConfig) -> Self {
        PolarRamp { transport, cfg }
    }

    fn auth_headers(&self) -> Vec<(String, String)> {
        let key = self.cfg.api_key.clone().unwrap_or_default();
        vec![("authorization".into(), format!("Bearer {key}"))]
    }

    /// Create a Polar checkout crediting `account_id` (threaded via `metadata`).
    pub async fn create_checkout(
        &self,
        account_id: &str,
        amount_micros: i64,
    ) -> Result<Checkout, RampError> {
        let body = serde_json::json!({
            "amount": amount_micros / 10_000,
            "currency": "usd",
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
            checkout_url: v["url"].as_str().unwrap_or_default().to_string(),
            checkout_id: v["id"].as_str().unwrap_or_default().to_string(),
        })
    }
}

#[async_trait]
impl Ramp for PolarRamp {
    async fn confirm_deposit(&self, deposit_ref: &str) -> Result<Deposit, RampError> {
        let url = format!("{API_BASE}/v1/orders/{deposit_ref}");
        let resp = map_status(self.transport.get(&url, &self.auth_headers()).await)?;
        let v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        let paid = v["paid"].as_bool().unwrap_or(false) || v["status"].as_str() == Some("paid");
        if !paid {
            return Err(RampError::NotConfirmed);
        }
        let cents = amount_cents(&v).ok_or(RampError::ProviderUnavailable)?;
        let micros = cents_to_micros(cents).ok_or(RampError::ProviderUnavailable)?;
        Ok(Deposit {
            deposit_ref: deposit_ref.to_string(),
            amount_micros: Micros::new(micros),
            asset: Asset::Usd,
            confirmed: true,
            bound_account: None,
        })
    }

    async fn initiate_payout(
        &self,
        _payout_ref: &str,
        _dest: &PayoutDest,
        _amount: Micros,
    ) -> Result<Payout, RampError> {
        Err(RampError::ProviderUnavailable)
    }
}

/// Verify a Polar (Standard Webhooks) delivery over RAW bytes.
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
    let id = headers
        .get_header("webhook-id")
        .ok_or(WebhookError::BadSignature)?;
    let ts = headers
        .get_header("webhook-timestamp")
        .ok_or(WebhookError::BadSignature)?;
    let sig = headers
        .get_header("webhook-signature")
        .ok_or(WebhookError::BadSignature)?;
    standard_webhooks_verify(secret, id, ts, raw, sig)?;

    let v: Value =
        serde_json::from_slice(raw).map_err(|e| WebhookError::Malformed(e.to_string()))?;
    let event_type = v["type"].as_str().unwrap_or("");
    let data = &v["data"];
    let crediting = match event_type {
        "order.paid" => true,
        "checkout.updated" => data["status"].as_str() == Some("succeeded"),
        _ => false,
    };
    if !crediting {
        return Err(WebhookError::Ignored(event_type.to_string()));
    }
    let deposit_ref = data["id"]
        .as_str()
        .ok_or_else(|| WebhookError::Malformed("missing data.id".into()))?
        .to_string();
    let cents =
        amount_cents(data).ok_or_else(|| WebhookError::Malformed("missing amount".into()))?;
    let amount_micros =
        cents_to_micros(cents).ok_or_else(|| WebhookError::Malformed("amount overflow".into()))?;
    let account_id = data["metadata"]["account_id"]
        .as_str()
        .ok_or_else(|| WebhookError::Malformed("missing metadata.account_id".into()))?
        .to_string();
    Ok(DepositEvent {
        provider: ProviderId::Polar,
        deposit_ref,
        amount_micros,
        asset: Asset::Usd,
        account_id,
    })
}

/// Polar reports the paid amount at `amount` / `total_amount` (minor units).
fn amount_cents(obj: &Value) -> Option<i64> {
    obj["amount"]
        .as_i64()
        .or_else(|| obj["total_amount"].as_i64())
}
