//! Incoming Stripe webhooks — signature verification + event parsing.
//!
//! Faithful port of `PaymentProviders::Stripe::ValidateIncomingWebhookService`
//! (`Stripe::Webhook::Signature.verify_header`) and `HandleEventService`'s event
//! mapping. The `Stripe-Signature` header is `t=<ts>,v1=<hex-hmac>`; the signed
//! payload is `"{ts}.{raw_body}"` HMAC-SHA256'd (hex) with the endpoint secret.

use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use thiserror::Error;

/// Default signature tolerance (seconds) — Stripe's `DEFAULT_TOLERANCE`.
pub const DEFAULT_TOLERANCE_SECS: i64 = 300;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum WebhookError {
    #[error("invalid signature header")]
    InvalidHeader,
    #[error("signature mismatch")]
    SignatureMismatch,
    #[error("timestamp outside tolerance")]
    TimestampOutOfTolerance,
    #[error("invalid payload: {0}")]
    InvalidPayload(String),
}

/// The subset of a Stripe event Rago acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StripeEvent {
    pub event_type: String,
    /// The event object id (e.g. the PaymentIntent id `pi_…`).
    pub object_id: String,
    /// The object status (e.g. `succeeded`).
    pub object_status: Option<String>,
    /// `metadata.lago_payment_id` etc.
    pub metadata: Value,
}

/// The payable-facing status an incoming event maps to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IncomingStatus {
    Succeeded,
    Failed,
    Refunded,
    Ignored,
}

impl StripeEvent {
    /// `HandleEventService::EVENT_MAPPING` — the payable status this event drives.
    pub fn incoming_status(&self) -> IncomingStatus {
        match self.event_type.as_str() {
            "payment_intent.succeeded" => IncomingStatus::Succeeded,
            "payment_intent.payment_failed" | "payment_intent.canceled" => IncomingStatus::Failed,
            "charge.refunded" | "charge.refund.updated" => IncomingStatus::Refunded,
            _ => IncomingStatus::Ignored,
        }
    }

    /// A refund-object event (`refund.created|updated|failed`,
    /// `charge.refund.updated`) as a refund notification.
    pub fn refund_event(&self) -> Option<super::provider_webhook::ProviderRefundEvent> {
        if !matches!(
            self.event_type.as_str(),
            "refund.created" | "refund.updated" | "refund.failed" | "charge.refund.updated"
        ) {
            return None;
        }
        let status = self.object_status.clone().unwrap_or_else(|| "pending".into());
        Some(super::provider_webhook::ProviderRefundEvent {
            provider_refund_id: self.object_id.clone(),
            state: super::stripe::stripe_refund_state(&status),
            provider_status: status,
        })
    }

    pub fn lago_payment_id(&self) -> Option<&str> {
        self.metadata.get("lago_payment_id").and_then(Value::as_str)
    }
    pub fn lago_payable_id(&self) -> Option<&str> {
        self.metadata.get("lago_payable_id").and_then(Value::as_str)
    }
}

/// `Stripe::Webhook::Signature.verify_header` — verify the `Stripe-Signature`
/// header over the raw body with the endpoint secret. `now` is the current
/// Unix time (injected for determinism).
pub fn verify_signature(
    payload: &str,
    signature_header: &str,
    secret: &str,
    now: i64,
    tolerance: i64,
) -> Result<(), WebhookError> {
    let mut timestamp: Option<i64> = None;
    let mut v1_signatures: Vec<String> = Vec::new();
    for part in signature_header.split(',') {
        let mut kv = part.splitn(2, '=');
        match (kv.next(), kv.next()) {
            (Some("t"), Some(v)) => timestamp = v.trim().parse().ok(),
            (Some("v1"), Some(v)) => v1_signatures.push(v.trim().to_string()),
            _ => {}
        }
    }
    let ts = timestamp.ok_or(WebhookError::InvalidHeader)?;
    if v1_signatures.is_empty() {
        return Err(WebhookError::InvalidHeader);
    }
    if (now - ts).abs() > tolerance {
        return Err(WebhookError::TimestampOutOfTolerance);
    }

    let expected = compute_signature(secret, ts, payload);
    if v1_signatures.iter().any(|s| constant_time_eq(s, &expected)) {
        Ok(())
    } else {
        Err(WebhookError::SignatureMismatch)
    }
}

/// Compute the `v1` signature hex for a payload/timestamp (also used to build
/// signed test requests).
pub fn compute_signature(secret: &str, timestamp: i64, payload: &str) -> String {
    let signed_payload = format!("{timestamp}.{payload}");
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC any key size");
    mac.update(signed_payload.as_bytes());
    hex(&mac.finalize().into_bytes())
}

/// Build a full `Stripe-Signature` header for a payload (for signed tests).
pub fn signature_header(secret: &str, timestamp: i64, payload: &str) -> String {
    format!("t={timestamp},v1={}", compute_signature(secret, timestamp, payload))
}

/// `Stripe::Event.construct_from(JSON.parse(body))` — parse the event object.
pub fn parse_event(payload: &str) -> Result<StripeEvent, WebhookError> {
    let json: Value = serde_json::from_str(payload).map_err(|e| WebhookError::InvalidPayload(e.to_string()))?;
    let event_type = json
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| WebhookError::InvalidPayload("missing type".into()))?
        .to_string();
    let object = json.pointer("/data/object").cloned().unwrap_or(Value::Null);
    let object_id = object
        .get("id")
        .and_then(Value::as_str)
        .ok_or_else(|| WebhookError::InvalidPayload("missing data.object.id".into()))?
        .to_string();
    let object_status = object.get("status").and_then(Value::as_str).map(str::to_string);
    let metadata = object.get("metadata").cloned().unwrap_or(Value::Null);
    Ok(StripeEvent {
        event_type,
        object_id,
        object_status,
        metadata,
    })
}

fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}
