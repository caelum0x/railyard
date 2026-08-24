//! PayPal adapter. On-ramp: order create + capture re-confirmation. Off-ramp:
//! Payouts API. Webhook verification is SERVER-SIDE (not a local HMAC): the
//! delivery is echoed back to PayPal's
//! `/v1/notifications/verify-webhook-signature` endpoint, so this provider's
//! `verify_webhook` is **async** and uses the injected [`HttpTransport`] (hence
//! fake-injectable in tests). Event `PAYMENT.CAPTURE.COMPLETED`.

use std::sync::Arc;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use mint_core::Micros;
use mint_reserve::{Asset, Deposit, Payout, PayoutDest, PayoutStatus, Ramp, RampError};
use serde_json::Value;

use crate::card::Checkout;
use crate::config::{Mode, ProviderConfig};
use crate::event::{DepositEvent, WebhookError};
use crate::http::{map_status, Headers, HttpTransport};
use crate::money::decimal_to_micros;
use crate::provider::ProviderId;

fn api_base(mode: Mode) -> &'static str {
    match mode {
        Mode::Sandbox => "https://api-m.sandbox.paypal.com",
        Mode::Live => "https://api-m.paypal.com",
    }
}

/// A live PayPal [`Ramp`] over an injected [`HttpTransport`].
pub struct PayPalRamp {
    transport: Arc<dyn HttpTransport>,
    cfg: ProviderConfig,
}

impl PayPalRamp {
    /// Build the adapter from its transport + resolved config.
    pub fn new(transport: Arc<dyn HttpTransport>, cfg: ProviderConfig) -> Self {
        PayPalRamp { transport, cfg }
    }

    fn base(&self) -> &'static str {
        api_base(self.cfg.mode)
    }

    /// Fetch an OAuth2 client-credentials access token.
    async fn access_token(&self) -> Result<String, RampError> {
        let client_id = self.cfg.api_key.clone().unwrap_or_default();
        let secret = self.cfg.extra("client_secret").unwrap_or_default();
        let basic = B64.encode(format!("{client_id}:{secret}"));
        let headers = vec![
            ("authorization".to_string(), format!("Basic {basic}")),
            (
                "content-type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            ),
        ];
        let url = format!("{}/v1/oauth2/token", self.base());
        let resp = map_status(
            self.transport
                .post_json(&url, &headers, b"grant_type=client_credentials")
                .await,
        )?;
        let v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        v["access_token"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or(RampError::ProviderUnavailable)
    }

    fn bearer(token: &str) -> Vec<(String, String)> {
        vec![
            ("authorization".into(), format!("Bearer {token}")),
            ("content-type".into(), "application/json".into()),
        ]
    }

    /// Create a PayPal order crediting `account_id` (threaded via `custom_id`).
    pub async fn create_checkout(
        &self,
        account_id: &str,
        amount_micros: i64,
    ) -> Result<Checkout, RampError> {
        let token = self.access_token().await?;
        let dollars = format!(
            "{}.{:02}",
            amount_micros / 1_000_000,
            (amount_micros % 1_000_000) / 10_000
        );
        let body = serde_json::json!({
            "intent": "CAPTURE",
            "purchase_units": [{
                "custom_id": account_id,
                "amount": { "currency_code": "USD", "value": dollars },
            }],
        });
        let url = format!("{}/v2/checkout/orders", self.base());
        let raw = serde_json::to_vec(&body).expect("json");
        let resp = map_status(
            self.transport
                .post_json(&url, &Self::bearer(&token), &raw)
                .await,
        )?;
        let v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        let checkout_id = v["id"].as_str().unwrap_or_default().to_string();
        let checkout_url = v["links"]
            .as_array()
            .and_then(|links| {
                links
                    .iter()
                    .find(|l| l["rel"].as_str() == Some("approve"))
                    .and_then(|l| l["href"].as_str())
            })
            .unwrap_or_default()
            .to_string();
        Ok(Checkout {
            checkout_url,
            checkout_id,
        })
    }

    /// Verify a PayPal webhook by echoing it back to PayPal's
    /// `verify-webhook-signature` endpoint (async, transport-backed).
    pub async fn verify_webhook<H: Headers>(
        &self,
        headers: &H,
        raw: &[u8],
    ) -> Result<DepositEvent, WebhookError> {
        let webhook_id = self
            .cfg
            .webhook_secret
            .as_deref()
            .filter(|s| !s.is_empty())
            .ok_or(WebhookError::NotConfigured)?;
        let event: Value =
            serde_json::from_slice(raw).map_err(|e| WebhookError::Malformed(e.to_string()))?;

        let get = |name: &str| headers.get_header(name).unwrap_or_default().to_string();
        let verify_body = serde_json::json!({
            "auth_algo": get("paypal-auth-algo"),
            "cert_url": get("paypal-cert-url"),
            "transmission_id": get("paypal-transmission-id"),
            "transmission_sig": get("paypal-transmission-sig"),
            "transmission_time": get("paypal-transmission-time"),
            "webhook_id": webhook_id,
            "webhook_event": event,
        });

        let token = self
            .access_token()
            .await
            .map_err(|_| WebhookError::Malformed("paypal token unavailable".into()))?;
        let url = format!("{}/v1/notifications/verify-webhook-signature", self.base());
        let raw_body = serde_json::to_vec(&verify_body).expect("json");
        let resp = map_status(
            self.transport
                .post_json(&url, &Self::bearer(&token), &raw_body)
                .await,
        )
        .map_err(|_| WebhookError::Malformed("paypal verify unavailable".into()))?;
        let vr: Value = serde_json::from_slice(&resp.body)
            .map_err(|e| WebhookError::Malformed(e.to_string()))?;
        if vr["verification_status"].as_str() != Some("SUCCESS") {
            return Err(WebhookError::BadSignature);
        }

        let event_type = event["event_type"].as_str().unwrap_or("");
        if event_type != "PAYMENT.CAPTURE.COMPLETED" {
            return Err(WebhookError::Ignored(event_type.to_string()));
        }
        let resource = &event["resource"];
        let deposit_ref = resource["id"]
            .as_str()
            .ok_or_else(|| WebhookError::Malformed("missing resource.id".into()))?
            .to_string();
        let value = resource["amount"]["value"]
            .as_str()
            .ok_or_else(|| WebhookError::Malformed("missing amount.value".into()))?;
        let amount_micros =
            decimal_to_micros(value).ok_or_else(|| WebhookError::Malformed("bad amount".into()))?;
        let account_id = resource["custom_id"]
            .as_str()
            .ok_or_else(|| WebhookError::Malformed("missing custom_id".into()))?
            .to_string();
        Ok(DepositEvent {
            provider: ProviderId::PayPal,
            deposit_ref,
            amount_micros,
            asset: Asset::Usd,
            account_id,
        })
    }
}

#[async_trait]
impl Ramp for PayPalRamp {
    async fn confirm_deposit(&self, deposit_ref: &str) -> Result<Deposit, RampError> {
        let token = self.access_token().await?;
        let url = format!("{}/v2/payments/captures/{deposit_ref}", self.base());
        let resp = map_status(self.transport.get(&url, &Self::bearer(&token)).await)?;
        let v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        let confirmed = v["status"].as_str() == Some("COMPLETED");
        if !confirmed {
            return Err(RampError::NotConfirmed);
        }
        let value = v["amount"]["value"]
            .as_str()
            .ok_or(RampError::ProviderUnavailable)?;
        let micros = decimal_to_micros(value).ok_or(RampError::ProviderUnavailable)?;
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
        payout_ref: &str,
        dest: &PayoutDest,
        amount: Micros,
    ) -> Result<Payout, RampError> {
        let receiver = match dest {
            // A PayPal payout targets an email / receiver id carried in the
            // `Stripe`-less `Crypto{address}` slot is invalid; use the account.
            PayoutDest::Stripe { account } => account.clone(),
            PayoutDest::Crypto { .. } => return Err(RampError::ProviderUnavailable),
        };
        let token = self.access_token().await?;
        let dollars = format!(
            "{}.{:02}",
            amount.get() / 1_000_000,
            (amount.get() % 1_000_000) / 10_000
        );
        let body = serde_json::json!({
            "sender_batch_header": { "sender_batch_id": payout_ref },
            "items": [{
                "recipient_type": "EMAIL",
                "amount": { "value": dollars, "currency": "USD" },
                "receiver": receiver,
            }],
        });
        let url = format!("{}/v1/payments/payouts", self.base());
        let raw = serde_json::to_vec(&body).expect("json");
        let resp = map_status(
            self.transport
                .post_json(&url, &Self::bearer(&token), &raw)
                .await,
        )?;
        let _v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        Ok(Payout {
            payout_ref: payout_ref.to_string(),
            amount_micros: amount,
            status: PayoutStatus::Paid,
        })
    }
}
