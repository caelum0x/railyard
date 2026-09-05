//! Stripe payment provider — faithful port of
//! `PaymentProviders::Stripe::Payments::CreateService` + `StripeProvider`.
//!
//! Creates a PaymentIntent (`POST /v1/payment_intents`, form-encoded, bearer
//! secret key, idempotency key `payment-<id>`) with `confirm=true`, then maps
//! the returned status:
//! * `succeeded` → [`PayableStatus::Succeeded`]
//! * `canceled` / `requires_payment_method` → [`PayableStatus::Failed`]
//! * anything else (processing, requires_action, …) → [`PayableStatus::Pending`]

use async_trait::async_trait;
use serde_json::Value;

use super::{PaymentError, PaymentOutcome, PaymentProvider, PaymentRequest, PayableStatus};
use crate::transport::HttpTransport;

const STRIPE_PAYMENT_INTENTS_URL: &str = "https://api.stripe.com/v1/payment_intents";
const SUCCESS_STATUSES: &[&str] = &["succeeded"];
const FAILED_STATUSES: &[&str] = &["canceled", "requires_payment_method"];

/// `PaymentProviders::Stripe` over an injectable transport `T`.
pub struct StripeProvider<T: HttpTransport> {
    secret_key: String,
    transport: T,
}

impl<T: HttpTransport> StripeProvider<T> {
    pub fn new(secret_key: impl Into<String>, transport: T) -> Self {
        Self {
            secret_key: secret_key.into(),
            transport,
        }
    }

    /// `StripeProvider#determine_payment_status`.
    pub fn determine_payment_status(status: &str) -> PayableStatus {
        if SUCCESS_STATUSES.contains(&status) {
            PayableStatus::Succeeded
        } else if FAILED_STATUSES.contains(&status) {
            PayableStatus::Failed
        } else {
            PayableStatus::Pending
        }
    }

    /// `#payment_intent_payload` — form-encoded PaymentIntent body.
    fn payment_intent_form(req: &PaymentRequest) -> String {
        let pairs: Vec<(String, String)> = vec![
            ("amount".into(), req.amount_cents.to_string()),
            ("currency".into(), req.currency.to_lowercase()),
            ("customer".into(), req.provider_customer_id.clone()),
            ("confirm".into(), "true".into()),
            ("off_session".into(), "true".into()),
            ("description".into(), req.reference.clone()),
            ("metadata[lago_payment_id]".into(), req.payment_id.to_string()),
            ("metadata[lago_payable_id]".into(), req.invoice_id.to_string()),
            ("metadata[lago_payable_type]".into(), "Invoice".into()),
            ("metadata[lago_customer_id]".into(), req.customer_id.to_string()),
        ];
        form_urlencode(&pairs)
    }
}

#[async_trait]
impl<T: HttpTransport> PaymentProvider for StripeProvider<T> {
    fn payment_type(&self) -> &'static str {
        "stripe"
    }

    async fn create_payment(&self, req: &PaymentRequest) -> Result<PaymentOutcome, PaymentError> {
        let headers = vec![
            (
                "Authorization".to_string(),
                format!("Bearer {}", self.secret_key),
            ),
            (
                "Content-Type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            ),
            (
                "Idempotency-Key".to_string(),
                format!("payment-{}", req.payment_id),
            ),
        ];
        let body = Self::payment_intent_form(req);

        let resp = self
            .transport
            .post(STRIPE_PAYMENT_INTENTS_URL, &headers, body)
            .await?;

        let json: Value = serde_json::from_str(&resp.body)
            .map_err(|e| PaymentError::InvalidResponse(e.to_string()))?;

        if !resp.is_success() {
            let err = json.get("error");
            return Err(PaymentError::Provider {
                code: err
                    .and_then(|e| e.get("code"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                message: err
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("stripe error")
                    .to_string(),
            });
        }

        let provider_payment_id = json
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| PaymentError::InvalidResponse("missing id".into()))?
            .to_string();
        let provider_status = json
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| PaymentError::InvalidResponse("missing status".into()))?
            .to_string();

        Ok(PaymentOutcome {
            payable_status: Self::determine_payment_status(&provider_status),
            provider_payment_id,
            provider_status,
        })
    }
}

/// Minimal `application/x-www-form-urlencoded` encoder (no extra deps).
fn form_urlencode(pairs: &[(String, String)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", percent_encode(k), percent_encode(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}
