//! Incoming webhooks for GoCardless and Adyen — signature verification + event
//! parsing that feed the *same* status-sync path as Stripe
//! ([`super::stripe_webhook::IncomingStatus`]).
//!
//! * **GoCardless** (`GoCardlessPro::Webhook.parse`): HMAC-SHA256 hex over the
//!   raw body with the endpoint secret, in the `Webhook-Signature` header. The
//!   body carries `{ events: [{ resource_type, action, links: { payment } }] }`.
//! * **Adyen**: HMAC-SHA256 (base64) over the canonical notification data
//!   string (`pspReference:originalReference:merchantAccountCode:merchantReference
//!   :value:currency:eventCode:success`), in `additionalData.hmacSignature`.

use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;

use super::stripe_webhook::{IncomingStatus, WebhookError};

/// One parsed provider webhook notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderWebhookEvent {
    /// Provider payment id the notification refers to.
    pub provider_payment_id: String,
    pub status: IncomingStatus,
}

// ---- GoCardless ------------------------------------------------------------

/// `GoCardlessPro::Webhook.parse` — verify the `Webhook-Signature` HMAC (hex)
/// over the raw body with the endpoint secret.
pub fn gocardless_verify(payload: &str, signature: &str, secret: &str) -> Result<(), WebhookError> {
    let expected = hmac_hex(secret, payload);
    if constant_time_eq(signature.trim(), &expected) {
        Ok(())
    } else {
        Err(WebhookError::SignatureMismatch)
    }
}

/// Parse GoCardless `{ events: [...] }`, mapping each event's `action` to a
/// payable status. Only payment-resource events are returned.
pub fn gocardless_parse(payload: &str) -> Result<Vec<ProviderWebhookEvent>, WebhookError> {
    let json: Value =
        serde_json::from_str(payload).map_err(|e| WebhookError::InvalidPayload(e.to_string()))?;
    let events = json.get("events").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut out = Vec::new();
    for ev in events {
        if ev.get("resource_type").and_then(Value::as_str) != Some("payments") {
            continue;
        }
        let Some(payment_id) = ev.pointer("/links/payment").and_then(Value::as_str) else {
            continue;
        };
        let action = ev.get("action").and_then(Value::as_str).unwrap_or("");
        let status = match action {
            "paid_out" | "confirmed" => IncomingStatus::Succeeded,
            "cancelled" | "failed" | "customer_approval_denied" | "charged_back" => {
                IncomingStatus::Failed
            }
            _ => IncomingStatus::Ignored,
        };
        out.push(ProviderWebhookEvent {
            provider_payment_id: payment_id.to_string(),
            status,
        });
    }
    Ok(out)
}

// ---- Adyen -----------------------------------------------------------------

/// Verify an Adyen notification-item HMAC (base64) over the canonical data
/// string. `item` is the `NotificationRequestItem` object.
pub fn adyen_verify(item: &Value, secret_hex: &str) -> Result<(), WebhookError> {
    let provided = item
        .pointer("/additionalData/hmacSignature")
        .and_then(Value::as_str)
        .ok_or(WebhookError::InvalidHeader)?;
    let expected = adyen_expected_signature(item, secret_hex)?;
    if constant_time_eq(provided, &expected) {
        Ok(())
    } else {
        Err(WebhookError::SignatureMismatch)
    }
}

/// Compute the Adyen HMAC signature (base64) for a notification item — also used
/// to build signed test notifications.
pub fn adyen_expected_signature(item: &Value, secret_hex: &str) -> Result<String, WebhookError> {
    let get = |p: &str| item.pointer(p).and_then(Value::as_str).unwrap_or("");
    let value = item
        .pointer("/amount/value")
        .and_then(Value::as_i64)
        .map(|v| v.to_string())
        .unwrap_or_default();
    let data = [
        get("/pspReference"),
        get("/originalReference"),
        get("/merchantAccountCode"),
        get("/merchantReference"),
        &value,
        item.pointer("/amount/currency").and_then(Value::as_str).unwrap_or(""),
        get("/eventCode"),
        get("/success"),
    ]
    .join(":");

    let key = hex_decode(secret_hex).ok_or(WebhookError::InvalidHeader)?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).map_err(|_| WebhookError::InvalidHeader)?;
    mac.update(data.as_bytes());
    Ok(base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes()))
}

/// Map an Adyen notification item to a status event.
pub fn adyen_event(item: &Value) -> ProviderWebhookEvent {
    let event_code = item.get("eventCode").and_then(Value::as_str).unwrap_or("");
    let success = item.get("success").and_then(Value::as_str) == Some("true");
    let status = match (event_code, success) {
        ("AUTHORISATION", true) => IncomingStatus::Succeeded,
        ("AUTHORISATION", false) => IncomingStatus::Failed,
        ("REFUND", _) => IncomingStatus::Refunded,
        ("CANCELLATION", _) => IncomingStatus::Failed,
        _ => IncomingStatus::Ignored,
    };
    ProviderWebhookEvent {
        provider_payment_id: item.get("pspReference").and_then(Value::as_str).unwrap_or("").to_string(),
        status,
    }
}

// ---- refunds ---------------------------------------------------------------

/// A provider notification about a refund Rago issued.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRefundEvent {
    pub provider_refund_id: String,
    pub state: super::RefundState,
    pub provider_status: String,
}

/// GoCardless `refunds` resource events (`paid`, `refund_settled`, `failed`,
/// `bounced`, `funds_returned`, ...).
pub fn gocardless_refund_events(payload: &str) -> Result<Vec<ProviderRefundEvent>, WebhookError> {
    let json: Value =
        serde_json::from_str(payload).map_err(|e| WebhookError::InvalidPayload(e.to_string()))?;
    let events = json.get("events").and_then(Value::as_array).cloned().unwrap_or_default();
    let mut out = Vec::new();
    for ev in events {
        if ev.get("resource_type").and_then(Value::as_str) != Some("refunds") {
            continue;
        }
        let Some(refund_id) = ev.pointer("/links/refund").and_then(Value::as_str) else {
            continue;
        };
        let action = ev.get("action").and_then(Value::as_str).unwrap_or("").to_string();
        let state = match action.as_str() {
            "paid" | "refund_settled" => super::RefundState::Succeeded,
            "failed" | "bounced" | "funds_returned" | "cancelled" => super::RefundState::Failed,
            _ => super::RefundState::Pending,
        };
        out.push(ProviderRefundEvent { provider_refund_id: refund_id.to_string(), state, provider_status: action });
    }
    Ok(out)
}

/// Adyen `REFUND` / `REFUND_FAILED` notification for a refund (its own
/// `pspReference`). `None` for any other event code.
pub fn adyen_refund_event(item: &Value) -> Option<ProviderRefundEvent> {
    let event_code = item.get("eventCode").and_then(Value::as_str).unwrap_or("");
    let success = item.get("success").and_then(Value::as_str) == Some("true");
    let state = match (event_code, success) {
        ("REFUND", true) => super::RefundState::Succeeded,
        ("REFUND", false) | ("REFUND_FAILED", _) => super::RefundState::Failed,
        _ => return None,
    };
    let id = item.get("pspReference").and_then(Value::as_str)?.to_string();
    Some(ProviderRefundEvent { provider_refund_id: id, state, provider_status: event_code.to_string() })
}

/// The payment PSP reference an Adyen modification (refund, cancellation)
/// applies to.
pub fn adyen_original_reference(item: &Value) -> Option<String> {
    item.get("originalReference").and_then(Value::as_str).map(str::to_string)
}

// ---- helpers ---------------------------------------------------------------

fn hmac_hex(secret: &str, payload: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC any key size");
    mac.update(payload.as_bytes());
    let bytes = mac.finalize().into_bytes();
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
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
