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
//! * **Creem**: HMAC-SHA256 (hex) over the raw body, in `creem-signature`.
//! * **Dodo**: Standard Webhooks: base64 HMAC-SHA256 over
//!   `"{webhook-id}.{webhook-timestamp}.{body}"`, secret `whsec_<base64>`,
//!   `webhook-signature: v1,<sig> ...`, five-minute replay window.
//! * **Iyzico**: merchant notifications carry `X-IYZ-SIGNATURE-V3` = hex
//!   HMAC-SHA256 (merchant secret key) over `secretKey + iyziEventType +
//!   paymentId [+ token] + paymentConversationId + status`.

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

// ---- Creem -----------------------------------------------------------------

/// Verify `creem-signature` (hex HMAC-SHA256 of the raw body).
pub fn creem_verify(payload: &str, signature: &str, secret: &str) -> Result<(), WebhookError> {
    if secret.is_empty() {
        return Err(WebhookError::InvalidHeader);
    }
    let expected = hmac_hex(secret, payload);
    if constant_time_eq(signature.trim(), &expected) {
        Ok(())
    } else {
        Err(WebhookError::SignatureMismatch)
    }
}

/// What a verified Creem event means for Rago.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreemWebhook {
    /// Creem event id (`evt_...`), for replay dedupe.
    pub event_id: Option<String>,
    pub event_type: String,
    /// Payment status change, keyed by the checkout id Rago stored.
    pub payment: Option<ProviderWebhookEvent>,
    /// Refund settlement, keyed by the transaction id Rago stored.
    pub refund: Option<ProviderRefundEvent>,
    /// The checkout a refund applies to (for refunds issued outside Rago).
    pub refund_checkout_id: Option<String>,
}

/// Parse a verified Creem event.
pub fn creem_parse(payload: &str) -> Result<CreemWebhook, WebhookError> {
    let v: Value =
        serde_json::from_str(payload).map_err(|e| WebhookError::InvalidPayload(e.to_string()))?;
    let event_type = v["eventType"].as_str().or_else(|| v["type"].as_str()).unwrap_or("").to_string();
    let obj = &v["object"];
    let mut out = CreemWebhook {
        event_id: v["id"].as_str().map(str::to_string),
        event_type: event_type.clone(),
        payment: None,
        refund: None,
        refund_checkout_id: None,
    };
    match event_type.as_str() {
        "checkout.completed" => {
            let id = obj["id"]
                .as_str()
                .ok_or_else(|| WebhookError::InvalidPayload("missing object.id".into()))?;
            let paid = obj["status"].as_str() == Some("completed")
                || obj["order"]["status"].as_str() == Some("paid");
            out.payment = Some(ProviderWebhookEvent {
                provider_payment_id: id.to_string(),
                status: if paid { IncomingStatus::Succeeded } else { IncomingStatus::Ignored },
            });
        }
        "refund.created" => {
            let transaction = super::creem::ref_id(&obj["transaction"])
                .ok_or_else(|| WebhookError::InvalidPayload("refund without transaction".into()))?;
            let status = obj["status"].as_str().unwrap_or("succeeded").to_string();
            out.refund = Some(ProviderRefundEvent {
                provider_refund_id: transaction,
                state: super::creem::refund_state(&status),
                provider_status: status,
            });
            out.refund_checkout_id = super::creem::ref_id(&obj["checkout"]);
        }
        _ => {}
    }
    Ok(out)
}

// ---- Dodo (Standard Webhooks) ---------------------------------------------

/// Dodo's replay window, seconds.
pub const DODO_TOLERANCE_SECS: i64 = 300;

/// Verify a Standard Webhooks delivery at time `now` (unix seconds).
pub fn dodo_verify(
    payload: &str,
    webhook_id: &str,
    timestamp: &str,
    signature_header: &str,
    secret: &str,
    now: i64,
) -> Result<(), WebhookError> {
    let ts: i64 = timestamp.trim().parse().map_err(|_| WebhookError::InvalidHeader)?;
    if (now - ts).abs() > DODO_TOLERANCE_SECS {
        return Err(WebhookError::TimestampOutOfTolerance);
    }
    let key_b64 = secret.trim().strip_prefix("whsec_").unwrap_or(secret.trim());
    let key = base64::engine::general_purpose::STANDARD
        .decode(key_b64)
        .map_err(|_| WebhookError::InvalidHeader)?;
    let expected = standard_webhooks_signature(&key, webhook_id, timestamp.trim(), payload);
    let matched = signature_header
        .split_whitespace()
        .map(|part| part.strip_prefix("v1,").unwrap_or(part))
        .any(|sig| constant_time_eq(sig, &expected));
    if matched {
        Ok(())
    } else {
        Err(WebhookError::SignatureMismatch)
    }
}

/// The base64 Standard Webhooks signature (also used to sign test deliveries).
pub fn standard_webhooks_signature(key: &[u8], webhook_id: &str, timestamp: &str, payload: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC any key size");
    mac.update(webhook_id.as_bytes());
    mac.update(b".");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(payload.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// What a verified Dodo event means for Rago.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DodoWebhook {
    pub event_type: String,
    /// Payment status change keyed by Dodo `payment_id`.
    pub payment: Option<ProviderWebhookEvent>,
    /// The checkout session the payment came from (to rebind a session id).
    pub checkout_session_id: Option<String>,
    /// `metadata.lago_invoice_id` (set on every Rago checkout).
    pub lago_invoice_id: Option<String>,
    pub refund: Option<ProviderRefundEvent>,
}

/// Parse a verified Dodo event.
pub fn dodo_parse(payload: &str) -> Result<DodoWebhook, WebhookError> {
    let v: Value =
        serde_json::from_str(payload).map_err(|e| WebhookError::InvalidPayload(e.to_string()))?;
    let event_type = v["type"].as_str().unwrap_or("").to_string();
    let data = &v["data"];
    let text = |key: &str| data[key].as_str().filter(|s| !s.is_empty()).map(str::to_string);
    let mut out = DodoWebhook {
        event_type: event_type.clone(),
        payment: None,
        checkout_session_id: text("checkout_session_id"),
        lago_invoice_id: data["metadata"]["lago_invoice_id"].as_str().map(str::to_string),
        refund: None,
    };
    let payment_status = match event_type.as_str() {
        "payment.succeeded" => Some(IncomingStatus::Succeeded),
        "payment.failed" | "payment.cancelled" => Some(IncomingStatus::Failed),
        _ => None,
    };
    if let Some(status) = payment_status {
        let payment_id = text("payment_id")
            .ok_or_else(|| WebhookError::InvalidPayload("missing data.payment_id".into()))?;
        out.payment = Some(ProviderWebhookEvent { provider_payment_id: payment_id, status });
    }
    if matches!(event_type.as_str(), "refund.succeeded" | "refund.failed") {
        let refund_id = text("refund_id")
            .ok_or_else(|| WebhookError::InvalidPayload("missing data.refund_id".into()))?;
        let status = text("status").unwrap_or_else(|| {
            if event_type == "refund.succeeded" { "succeeded".into() } else { "failed".into() }
        });
        out.refund = Some(ProviderRefundEvent {
            provider_refund_id: refund_id,
            state: super::dodo::refund_state(&status),
            provider_status: status,
        });
    }
    Ok(out)
}

// ---- Iyzico merchant notifications -----------------------------------------

/// A verified Iyzico merchant notification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IyzicoNotification {
    pub event_type: String,
    pub payment_id: String,
    /// Checkout-form token (hosted payments).
    pub token: Option<String>,
    /// `paymentConversationId` (the Rago payment id Rago sent).
    pub conversation_id: String,
    pub status: String,
}

impl IyzicoNotification {
    /// The payable status a payment notification drives. Refund / cancel
    /// notifications are `Refunded` only when successful.
    pub fn incoming_status(&self) -> IncomingStatus {
        let kind = self.event_type.to_ascii_uppercase();
        let success = self.status.eq_ignore_ascii_case("SUCCESS");
        if kind.contains("REFUND") || kind.contains("CANCEL") {
            return if success { IncomingStatus::Refunded } else { IncomingStatus::Ignored };
        }
        if kind.contains("CHARGEBACK") || kind.contains("DISPUTE") {
            return IncomingStatus::Ignored;
        }
        match self.status.to_ascii_uppercase().as_str() {
            "SUCCESS" => IncomingStatus::Succeeded,
            "FAILURE" => IncomingStatus::Failed,
            _ => IncomingStatus::Ignored,
        }
    }
}

/// Verify `X-IYZ-SIGNATURE-V3` and parse the notification.
pub fn iyzico_verify_notification(
    payload: &str,
    signature: &str,
    secret_key: &str,
) -> Result<IyzicoNotification, WebhookError> {
    if secret_key.is_empty() {
        return Err(WebhookError::InvalidHeader);
    }
    let v: Value =
        serde_json::from_str(payload).map_err(|e| WebhookError::InvalidPayload(e.to_string()))?;
    let text = |k: &str| match &v[k] {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => String::new(),
    };
    let event_type = text("iyziEventType");
    let payment_id = match text("paymentId") {
        p if p.is_empty() => text("iyziPaymentId"),
        p => p,
    };
    let conversation_id = text("paymentConversationId");
    let status = text("status");
    let token = text("token");
    let message = if token.is_empty() {
        format!("{secret_key}{event_type}{payment_id}{conversation_id}{status}")
    } else {
        format!("{secret_key}{event_type}{payment_id}{token}{conversation_id}{status}")
    };
    if !constant_time_eq(&signature.trim().to_ascii_lowercase(), &hmac_hex(secret_key, &message)) {
        return Err(WebhookError::SignatureMismatch);
    }
    Ok(IyzicoNotification {
        event_type,
        payment_id,
        token: (!token.is_empty()).then_some(token),
        conversation_id,
        status,
    })
}

/// Sign an Iyzico notification (tests and local replay tooling).
pub fn iyzico_notification_signature(secret_key: &str, event_type: &str, payment_id: &str, token: Option<&str>, conversation_id: &str, status: &str) -> String {
    let message = format!(
        "{secret_key}{event_type}{payment_id}{}{conversation_id}{status}",
        token.unwrap_or("")
    );
    hmac_hex(secret_key, &message)
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

pub(crate) fn hmac_hex(secret: &str, payload: &str) -> String {
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

pub(crate) fn constant_time_eq(a: &str, b: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creem_checkout_and_refund_events_verify_and_parse() {
        let body = r#"{"id":"evt_1","eventType":"checkout.completed","object":{"id":"ch_1","status":"completed","order":{"id":"ord_1","status":"paid"}}}"#;
        let sig = hmac_hex("whsec_creem", body);
        assert!(creem_verify(body, &sig, "whsec_creem").is_ok());
        assert_eq!(creem_verify(body, &sig, "other"), Err(WebhookError::SignatureMismatch));
        let parsed = creem_parse(body).unwrap();
        assert_eq!(parsed.event_id.as_deref(), Some("evt_1"));
        assert_eq!(
            parsed.payment,
            Some(ProviderWebhookEvent { provider_payment_id: "ch_1".into(), status: IncomingStatus::Succeeded })
        );

        let refund = r#"{"eventType":"refund.created","object":{"id":"ref_1","status":"succeeded","transaction":{"id":"tran_1"},"checkout":{"id":"ch_1"}}}"#;
        let parsed = creem_parse(refund).unwrap();
        let ev = parsed.refund.unwrap();
        assert_eq!(ev.provider_refund_id, "tran_1");
        assert_eq!(ev.state, super::super::RefundState::Succeeded);
        assert_eq!(parsed.refund_checkout_id.as_deref(), Some("ch_1"));
    }

    #[test]
    fn dodo_standard_webhooks_verify_with_replay_window() {
        use base64::Engine;
        let key = b"dodo-signing-key";
        let secret = format!("whsec_{}", base64::engine::general_purpose::STANDARD.encode(key));
        let body = r#"{"type":"payment.succeeded","data":{"payment_id":"pay_1","checkout_session_id":"cks_1","metadata":{"lago_invoice_id":"inv"}}}"#;
        let sig = format!("v1,{}", standard_webhooks_signature(key, "msg_1", "1000", body));
        assert!(dodo_verify(body, "msg_1", "1000", &sig, &secret, 1100).is_ok());
        assert_eq!(
            dodo_verify(body, "msg_1", "1000", &sig, &secret, 1000 + DODO_TOLERANCE_SECS + 1),
            Err(WebhookError::TimestampOutOfTolerance)
        );
        assert_eq!(
            dodo_verify(body, "msg_2", "1000", &sig, &secret, 1000),
            Err(WebhookError::SignatureMismatch)
        );
        let parsed = dodo_parse(body).unwrap();
        assert_eq!(parsed.payment.unwrap().provider_payment_id, "pay_1");
        assert_eq!(parsed.checkout_session_id.as_deref(), Some("cks_1"));
        assert_eq!(parsed.lago_invoice_id.as_deref(), Some("inv"));

        let refund = r#"{"type":"refund.failed","data":{"refund_id":"ref_1","payment_id":"pay_1","status":"failed"}}"#;
        let ev = dodo_parse(refund).unwrap().refund.unwrap();
        assert_eq!(ev.state, super::super::RefundState::Failed);
    }

    #[test]
    fn iyzico_notification_signature_covers_every_field() {
        let secret = "iyz-secret";
        let sig = iyzico_notification_signature(secret, "CHECKOUT_FORM_AUTH", "123", Some("tok"), "conv", "SUCCESS");
        let body = r#"{"iyziEventType":"CHECKOUT_FORM_AUTH","paymentId":123,"token":"tok","paymentConversationId":"conv","status":"SUCCESS"}"#;
        let n = iyzico_verify_notification(body, &sig, secret).unwrap();
        assert_eq!(n.payment_id, "123");
        assert_eq!(n.incoming_status(), IncomingStatus::Succeeded);
        let tampered = body.replace("SUCCESS", "FAILURE");
        assert_eq!(
            iyzico_verify_notification(&tampered, &sig, secret),
            Err(WebhookError::SignatureMismatch)
        );
        let refund = IyzicoNotification {
            event_type: "REFUND".into(),
            payment_id: "1".into(),
            token: None,
            conversation_id: String::new(),
            status: "SUCCESS".into(),
        };
        assert_eq!(refund.incoming_status(), IncomingStatus::Refunded);
    }
}
