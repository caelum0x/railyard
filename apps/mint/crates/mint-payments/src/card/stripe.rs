//! Stripe adapter. On-ramp: `checkout.sessions` create + `payment_intent`
//! re-confirmation. Off-ramp: `transfers`. Webhook: `Stripe-Signature`
//! (`t=..,v1=hex`), HMAC-SHA256 over `"{t}.{body}"`, event
//! `payment_intent.succeeded`; `cents × 10_000 = µ$`.

use std::sync::Arc;

use async_trait::async_trait;
use mint_core::Micros;
use mint_reserve::{Asset, Deposit, Payout, PayoutDest, PayoutStatus, Ramp, RampError};
use serde_json::Value;

use crate::card::{is_whole_cents, Checkout, CheckoutUrls};
use crate::config::ProviderConfig;
use crate::event::{DepositEvent, WebhookError};
use crate::http::{map_status, Headers, HttpTransport};
use crate::money::cents_to_micros;
use crate::provider::ProviderId;
use crate::verify::hmac_sha256_hex_verify;

const API_BASE: &str = "https://api.stripe.com";

/// A live Stripe [`Ramp`] over an injected [`HttpTransport`].
pub struct StripeRamp {
    transport: Arc<dyn HttpTransport>,
    cfg: ProviderConfig,
}

impl StripeRamp {
    /// Build the adapter from its transport + resolved config.
    pub fn new(transport: Arc<dyn HttpTransport>, cfg: ProviderConfig) -> Self {
        StripeRamp { transport, cfg }
    }

    fn auth_headers(&self) -> Vec<(String, String)> {
        let key = self.cfg.api_key.clone().unwrap_or_default();
        vec![("authorization".into(), format!("Bearer {key}"))]
    }

    /// Create a hosted Checkout Session crediting `account_id` for
    /// `amount_micros`. Sets `metadata[account_id]` so the webhook can resolve
    /// the mint target. Returns the hosted `checkout_url`.
    pub async fn create_checkout(
        &self,
        account_id: &str,
        amount_micros: i64,
        urls: &CheckoutUrls,
    ) -> Result<Checkout, RampError> {
        let cents = amount_micros / 10_000;
        let form = form_urlencode(&[
            ("mode", "payment"),
            ("success_url", &urls.success_url),
            ("cancel_url", &urls.cancel_url),
            ("line_items[0][quantity]", "1"),
            ("line_items[0][price_data][currency]", "usd"),
            (
                "line_items[0][price_data][product_data][name]",
                "AIT deposit",
            ),
            ("line_items[0][price_data][unit_amount]", &cents.to_string()),
            ("metadata[account_id]", account_id),
            ("payment_intent_data[metadata][account_id]", account_id),
        ]);
        let mut headers = self.auth_headers();
        headers.push((
            "content-type".into(),
            "application/x-www-form-urlencoded".into(),
        ));
        let url = format!("{API_BASE}/v1/checkout/sessions");
        let resp = map_status(
            self.transport
                .post_json(&url, &headers, form.as_bytes())
                .await,
        )?;
        let v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        let checkout_url = v["url"].as_str().unwrap_or_default().to_string();
        let checkout_id = v["id"].as_str().unwrap_or_default().to_string();
        Ok(Checkout {
            checkout_url,
            checkout_id,
        })
    }
}

#[async_trait]
impl Ramp for StripeRamp {
    async fn confirm_deposit(&self, deposit_ref: &str) -> Result<Deposit, RampError> {
        // Re-confirm the PaymentIntent against Stripe rather than trusting the
        // webhook JSON: the deposit is credited only if Stripe still reports it.
        let url = format!("{API_BASE}/v1/payment_intents/{deposit_ref}");
        let resp = map_status(self.transport.get(&url, &self.auth_headers()).await)?;
        let v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        let status = v["status"].as_str().unwrap_or("");
        let confirmed = status == "succeeded";
        let cents = v["amount_received"]
            .as_i64()
            .or_else(|| v["amount"].as_i64())
            .ok_or(RampError::ProviderUnavailable)?;
        let micros = cents_to_micros(cents).ok_or(RampError::ProviderUnavailable)?;
        if !confirmed {
            return Err(RampError::NotConfirmed);
        }
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
        self.check_payout(payout_ref, dest, amount).await?;
        let account = match dest {
            PayoutDest::Stripe { account } => account.clone(),
            _ => return Err(RampError::Unsupported),
        };
        let cents = (amount.get() / 10_000).to_string();
        let form = form_urlencode(&[
            ("amount", &cents),
            ("currency", "usd"),
            ("destination", &account),
        ]);
        let mut headers = self.auth_headers();
        // Stripe idempotency: keying on payout_ref makes retries exactly-once.
        headers.push(("idempotency-key".into(), payout_ref.to_string()));
        headers.push((
            "content-type".into(),
            "application/x-www-form-urlencoded".into(),
        ));
        let url = format!("{API_BASE}/v1/transfers");
        let resp = map_status(
            self.transport
                .post_json(&url, &headers, form.as_bytes())
                .await,
        )?;
        let v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        // A 2xx without a transfer id is not proof of a transfer.
        let transfer_id = v["id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or(RampError::ProviderUnavailable)?
            .to_string();
        // Stripe transfers to a connected account settle synchronously into its
        // balance; a transfer already reversed did not deliver funds.
        let status = if v["reversed"].as_bool() == Some(true) {
            PayoutStatus::Failed
        } else {
            PayoutStatus::Paid
        };
        Ok(Payout {
            payout_ref: payout_ref.to_string(),
            amount_micros: amount,
            status,
            provider_ref: Some(transfer_id),
        })
    }

    async fn check_payout(
        &self,
        _payout_ref: &str,
        dest: &PayoutDest,
        amount: Micros,
    ) -> Result<(), RampError> {
        match dest {
            PayoutDest::Stripe { account } if account.starts_with("acct_") => {}
            _ => return Err(RampError::Unsupported),
        }
        if !is_whole_cents(amount.get()) {
            return Err(RampError::Unsupported);
        }
        Ok(())
    }
}

/// Verify a Stripe webhook over RAW bytes, returning the confirmed deposit.
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
    let sig_header = headers
        .get_header("stripe-signature")
        .ok_or(WebhookError::BadSignature)?;
    let (t, v1) = parse_stripe_sig(sig_header).ok_or(WebhookError::BadSignature)?;

    let mut message = Vec::with_capacity(t.len() + 1 + raw.len());
    message.extend_from_slice(t.as_bytes());
    message.push(b'.');
    message.extend_from_slice(raw);
    hmac_sha256_hex_verify(secret.as_bytes(), &message, v1)?;

    let v: Value =
        serde_json::from_slice(raw).map_err(|e| WebhookError::Malformed(e.to_string()))?;
    let event_type = v["type"].as_str().unwrap_or("");
    if event_type != "payment_intent.succeeded" {
        return Err(WebhookError::Ignored(event_type.to_string()));
    }
    let obj = &v["data"]["object"];
    let deposit_ref = obj["id"]
        .as_str()
        .ok_or_else(|| WebhookError::Malformed("missing payment_intent id".into()))?
        .to_string();
    let cents = obj["amount_received"]
        .as_i64()
        .or_else(|| obj["amount"].as_i64())
        .ok_or_else(|| WebhookError::Malformed("missing amount".into()))?;
    let amount_micros =
        cents_to_micros(cents).ok_or_else(|| WebhookError::Malformed("amount overflow".into()))?;
    let account_id = obj["metadata"]["account_id"]
        .as_str()
        .ok_or_else(|| WebhookError::Malformed("missing metadata.account_id".into()))?
        .to_string();
    Ok(DepositEvent {
        provider: ProviderId::Stripe,
        deposit_ref,
        amount_micros,
        asset: Asset::Usd,
        account_id,
    })
}

/// Parse a `Stripe-Signature` header into `(t, v1)`.
fn parse_stripe_sig(header: &str) -> Option<(&str, &str)> {
    let mut t = None;
    let mut v1 = None;
    for part in header.split(',') {
        let mut kv = part.splitn(2, '=');
        match (kv.next(), kv.next()) {
            (Some("t"), Some(val)) => t = Some(val.trim()),
            (Some("v1"), Some(val)) => v1 = Some(val.trim()),
            _ => {}
        }
    }
    Some((t?, v1?))
}

/// Minimal `application/x-www-form-urlencoded` encoder (percent-encodes the
/// value bytes that Stripe forms care about).
fn form_urlencode(pairs: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (i, (k, v)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push('&');
        }
        out.push_str(&percent_encode(k));
        out.push('=');
        out.push_str(&percent_encode(v));
    }
    out
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
