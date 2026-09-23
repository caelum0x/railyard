//! PayPal adapter. On-ramp: order create (with a return URL pointing at Mint's
//! own `/checkout/paypal/return`), server-side order capture once the buyer
//! approves, and ORDER re-confirmation (the deposit ref is always the order id,
//! so the checkout response, the capture return and the
//! `PAYMENT.CAPTURE.COMPLETED` webhook all mint under ONE idempotency key).
//! Off-ramp: Payouts API (asynchronous — recorded `Pending` and polled). Webhook verification is SERVER-SIDE (not a local HMAC): the
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

use crate::card::{is_whole_cents, Checkout, CheckoutUrls};
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
    /// The buyer approves on PayPal and is sent to `urls.capture_return_url`
    /// (Mint's capture endpoint), which captures the order and mints.
    pub async fn create_checkout(
        &self,
        account_id: &str,
        amount_micros: i64,
        urls: &CheckoutUrls,
    ) -> Result<Checkout, RampError> {
        let return_url = urls
            .capture_return_url
            .as_deref()
            .ok_or(RampError::Unsupported)?;
        let token = self.access_token().await?;
        let body = serde_json::json!({
            "intent": "CAPTURE",
            "purchase_units": [{
                "custom_id": account_id,
                "description": "AIT deposit",
                "amount": { "currency_code": "USD", "value": micros_to_dollars(amount_micros) },
            }],
            "application_context": {
                "return_url": return_url,
                "cancel_url": urls.cancel_url,
                "user_action": "PAY_NOW",
                "shipping_preference": "NO_SHIPPING",
                "brand_name": "Mint",
            },
        });
        let url = format!("{}/v2/checkout/orders", self.base());
        let raw = serde_json::to_vec(&body).expect("json");
        let mut headers = Self::bearer(&token);
        // Idempotent order creation is not required (a duplicate order is simply
        // never approved), but PayPal-Request-Id makes client retries safe.
        headers.push((
            "paypal-request-id".into(),
            format!("mint-order-{account_id}-{amount_micros}-{}", ulid_like()),
        ));
        let resp = map_status(self.transport.post_json(&url, &headers, &raw).await)?;
        let v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        let checkout_id = v["id"].as_str().unwrap_or_default().to_string();
        let checkout_url = v["links"]
            .as_array()
            .and_then(|links| {
                links
                    .iter()
                    .find(|l| matches!(l["rel"].as_str(), Some("approve" | "payer-action")))
                    .and_then(|l| l["href"].as_str())
            })
            .unwrap_or_default()
            .to_string();
        if checkout_id.is_empty() || checkout_url.is_empty() {
            return Err(RampError::ProviderUnavailable);
        }
        Ok(Checkout {
            checkout_url,
            checkout_id,
        })
    }

    /// Capture an APPROVED order (the buyer returned from PayPal). Idempotent:
    /// the `PayPal-Request-Id` is the order id, and an already-captured order
    /// (a replayed return, or the webhook raced us) still resolves because the
    /// outcome is always read back from the authenticated order state.
    pub async fn capture_order(&self, order_id: &str) -> Result<Deposit, RampError> {
        if order_id.is_empty()
            || !order_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(RampError::DepositNotFound);
        }
        let token = self.access_token().await?;
        let url = format!("{}/v2/checkout/orders/{order_id}/capture", self.base());
        let mut headers = Self::bearer(&token);
        headers.push(("paypal-request-id".into(), format!("mint-capture-{order_id}")));
        // The capture response itself is not trusted: a 422
        // ORDER_ALREADY_CAPTURED is a success for us, and a 2xx is re-read below.
        let _ = self.transport.post_json(&url, &headers, b"{}").await;
        self.confirm_deposit(order_id).await
    }

    /// Read back an order's authenticated state (status, captured amount, the
    /// `custom_id` account binding).
    async fn fetch_order(&self, order_id: &str) -> Result<Value, RampError> {
        let token = self.access_token().await?;
        let url = format!("{}/v2/checkout/orders/{order_id}", self.base());
        let resp = map_status(self.transport.get(&url, &Self::bearer(&token)).await)?;
        serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)
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
        // Mint under the ORDER id (not the capture id) so the webhook, the
        // capture-return endpoint and the checkout response share one key.
        let deposit_ref = resource["supplementary_data"]["related_ids"]["order_id"]
            .as_str()
            .ok_or_else(|| {
                WebhookError::Malformed("missing supplementary_data.related_ids.order_id".into())
            })?
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
        // `deposit_ref` is the PayPal ORDER id. Credit only a COMPLETED order
        // whose first capture is COMPLETED; the authenticated `custom_id` binds
        // the credit to the account that created the checkout.
        let v = self.fetch_order(deposit_ref).await?;
        if v["status"].as_str() != Some("COMPLETED") {
            return Err(RampError::NotConfirmed);
        }
        let unit = &v["purchase_units"][0];
        let capture = &unit["payments"]["captures"][0];
        if capture["status"].as_str() != Some("COMPLETED") {
            return Err(RampError::NotConfirmed);
        }
        let value = capture["amount"]["value"]
            .as_str()
            .ok_or(RampError::ProviderUnavailable)?;
        let micros = decimal_to_micros(value).ok_or(RampError::ProviderUnavailable)?;
        Ok(Deposit {
            deposit_ref: deposit_ref.to_string(),
            amount_micros: Micros::new(micros),
            asset: Asset::Usd,
            confirmed: true,
            bound_account: unit["custom_id"].as_str().map(str::to_string),
        })
    }

    async fn initiate_payout(
        &self,
        payout_ref: &str,
        dest: &PayoutDest,
        amount: Micros,
    ) -> Result<Payout, RampError> {
        self.check_payout(payout_ref, dest, amount).await?;
        let receiver = match dest {
            PayoutDest::PayPal { email } => email.clone(),
            _ => return Err(RampError::Unsupported),
        };
        let token = self.access_token().await?;
        let body = serde_json::json!({
            // PayPal dedupes on sender_batch_id, so a retry with the same
            // payout_ref never pays twice.
            "sender_batch_header": {
                "sender_batch_id": payout_ref,
                "email_subject": "You have a payout",
            },
            "items": [{
                "recipient_type": "EMAIL",
                "amount": { "value": micros_to_dollars(amount.get()), "currency": "USD" },
                "receiver": receiver,
                "sender_item_id": payout_ref,
            }],
        });
        let url = format!("{}/v1/payments/payouts", self.base());
        let raw = serde_json::to_vec(&body).expect("json");
        let resp = map_status(
            self.transport
                .post_json(&url, &Self::bearer(&token), &raw)
                .await,
        )?;
        let v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        let header = &v["batch_header"];
        let batch_id = header["payout_batch_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or(RampError::ProviderUnavailable)?
            .to_string();
        // Payouts are asynchronous: a freshly created batch is PENDING until
        // PayPal settles each item. Only an outright DENIED batch fails now.
        let status = match header["batch_status"].as_str() {
            Some("DENIED") | Some("CANCELED") => PayoutStatus::Failed,
            Some("SUCCESS") => PayoutStatus::Paid,
            _ => PayoutStatus::Pending,
        };
        Ok(Payout {
            payout_ref: payout_ref.to_string(),
            amount_micros: amount,
            status,
            provider_ref: Some(batch_id),
        })
    }

    async fn check_payout(
        &self,
        _payout_ref: &str,
        dest: &PayoutDest,
        amount: Micros,
    ) -> Result<(), RampError> {
        match dest {
            PayoutDest::PayPal { email } if looks_like_email(email) => {}
            _ => return Err(RampError::Unsupported),
        }
        if !is_whole_cents(amount.get()) {
            return Err(RampError::Unsupported);
        }
        Ok(())
    }

    async fn payout_status(
        &self,
        _payout_ref: &str,
        provider_ref: Option<&str>,
    ) -> Result<PayoutStatus, RampError> {
        let batch_id = provider_ref
            .filter(|id| !id.is_empty())
            .ok_or(RampError::Unsupported)?;
        let token = self.access_token().await?;
        let url = format!("{}/v1/payments/payouts/{batch_id}", self.base());
        let resp = map_status(self.transport.get(&url, &Self::bearer(&token)).await)?;
        let v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        // One item per batch: its transaction_status is authoritative.
        if let Some(item_status) = v["items"][0]["transaction_status"].as_str() {
            return Ok(match item_status {
                "SUCCESS" => PayoutStatus::Paid,
                "FAILED" | "RETURNED" | "BLOCKED" | "REFUNDED" | "REVERSED" => {
                    PayoutStatus::Failed
                }
                // PENDING / ONHOLD / UNCLAIMED (receiver has 30 days) / NEW.
                _ => PayoutStatus::Pending,
            });
        }
        Ok(match v["batch_header"]["batch_status"].as_str() {
            Some("SUCCESS") => PayoutStatus::Paid,
            Some("DENIED") | Some("CANCELED") => PayoutStatus::Failed,
            _ => PayoutStatus::Pending,
        })
    }
}

/// Microdollars → a PayPal decimal dollar string (`"12.34"`). Callers have
/// already enforced whole cents.
fn micros_to_dollars(micros: i64) -> String {
    format!("{}.{:02}", micros / 1_000_000, (micros % 1_000_000) / 10_000)
}

/// Minimal structural email check for a PayPal receiver.
fn looks_like_email(s: &str) -> bool {
    let s = s.trim();
    match s.split_once('@') {
        Some((local, domain)) => {
            !local.is_empty() && domain.contains('.') && !domain.starts_with('.') && s.len() <= 254
        }
        None => false,
    }
}

/// A process-unique suffix for PayPal-Request-Id on order creation.
fn ulid_like() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{ms}-{}", SEQ.fetch_add(1, Ordering::Relaxed))
}
