//! GoCardless payment provider — faithful port of
//! `PaymentProviders::Gocardless::Payments::CreateService` + `GocardlessProvider`.
//!
//! Creates a direct-debit payment linked to a mandate
//! (`POST /payments`, bearer access token, `GoCardless-Version` header) and maps
//! the returned status:
//! * `paid_out` → [`PayableStatus::Succeeded`]
//! * `cancelled` / `customer_approval_denied` / `failed` / `charged_back` →
//!   [`PayableStatus::Failed`]
//! * anything else (pending_*, submitted, confirmed, …) → [`PayableStatus::Pending`]

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{
    PaymentError, PaymentOutcome, PaymentProvider, PaymentRequest, PayableStatus, RefundOutcome,
    RefundRequest, RefundState,
};
use crate::transport::HttpTransport;

const GOCARDLESS_PAYMENTS_URL: &str = "https://api.gocardless.com/payments";
const GOCARDLESS_REFUNDS_URL: &str = "https://api.gocardless.com/refunds";
const GOCARDLESS_VERSION: &str = "2015-07-06";
const SUCCESS_STATUSES: &[&str] = &["paid_out"];
const FAILED_STATUSES: &[&str] = &["cancelled", "customer_approval_denied", "failed", "charged_back"];

/// `PaymentProviders::Gocardless` over an injectable transport `T`.
pub struct GocardlessProvider<T: HttpTransport> {
    access_token: String,
    transport: T,
    attempt: u32,
}

impl<T: HttpTransport> GocardlessProvider<T> {
    pub fn new(access_token: impl Into<String>, transport: T) -> Self {
        Self {
            access_token: access_token.into(),
            transport,
            attempt: 0,
        }
    }

    /// Use attempt `n` of this payment in the idempotency key (`0` = first), so
    /// a retry of a failed payment is not replayed from the provider's cache.
    pub fn with_attempt(mut self, attempt: u32) -> Self {
        self.attempt = attempt;
        self
    }

    /// `GocardlessProvider#determine_payment_status`.
    pub fn determine_payment_status(status: &str) -> PayableStatus {
        if SUCCESS_STATUSES.contains(&status) {
            PayableStatus::Succeeded
        } else if FAILED_STATUSES.contains(&status) {
            PayableStatus::Failed
        } else {
            PayableStatus::Pending
        }
    }
}

#[async_trait]
impl<T: HttpTransport> PaymentProvider for GocardlessProvider<T> {
    fn payment_type(&self) -> &'static str {
        "gocardless"
    }

    async fn create_payment(&self, req: &PaymentRequest) -> Result<PaymentOutcome, PaymentError> {
        let mandate = req
            .provider_mandate_id
            .as_deref()
            .ok_or_else(|| PaymentError::Provider {
                code: Some("no_mandate_error".into()),
                message: "No mandate available for payment".into(),
            })?;

        let headers = vec![
            ("Authorization".to_string(), format!("Bearer {}", self.access_token)),
            ("GoCardless-Version".to_string(), GOCARDLESS_VERSION.to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Idempotency-Key".to_string(), super::idempotency_key(&req.payment_id, self.attempt)),
        ];
        let body = json!({
            "payments": {
                "amount": req.amount_cents,
                "currency": req.currency.to_uppercase(),
                "links": { "mandate": mandate },
                "metadata": {
                    "lago_payment_id": req.payment_id.to_string(),
                    "lago_invoice_id": req.invoice_id.to_string(),
                    "lago_customer_id": req.customer_id.to_string(),
                },
            }
        })
        .to_string();

        let resp = self.transport.post(GOCARDLESS_PAYMENTS_URL, &headers, body).await?;
        let json: Value = serde_json::from_str(&resp.body)
            .map_err(|e| PaymentError::InvalidResponse(e.to_string()))?;

        if !resp.is_success() {
            let err = json.pointer("/error");
            return Err(PaymentError::Provider {
                code: err.and_then(|e| e.get("type")).and_then(Value::as_str).map(str::to_string),
                message: err
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("gocardless error")
                    .to_string(),
            });
        }

        let payment = json.pointer("/payments").unwrap_or(&Value::Null);
        let provider_payment_id = payment
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| PaymentError::InvalidResponse("missing payments.id".into()))?
            .to_string();
        let provider_status = payment
            .get("status")
            .and_then(Value::as_str)
            .ok_or_else(|| PaymentError::InvalidResponse("missing payments.status".into()))?
            .to_string();

        Ok(PaymentOutcome {
            payable_status: Self::determine_payment_status(&provider_status),
            provider_payment_id,
            provider_status,
        })
    }

    /// `PaymentProviders::Gocardless::Refunds::CreateService` — `POST /refunds`
    /// linked to the payment, with the cumulative `total_amount_confirmation`
    /// GoCardless requires as a double-refund guard.
    async fn refund_payment(&self, req: &RefundRequest) -> Result<RefundOutcome, PaymentError> {
        let headers = vec![
            ("Authorization".to_string(), format!("Bearer {}", self.access_token)),
            ("GoCardless-Version".to_string(), GOCARDLESS_VERSION.to_string()),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Idempotency-Key".to_string(), super::refund_idempotency_key(&req.refund_id)),
        ];
        let body = json!({
            "refunds": {
                "amount": req.amount_cents,
                "total_amount_confirmation": req.total_refunded_cents,
                "links": { "payment": req.provider_payment_id },
                "metadata": { "lago_refund_id": req.refund_id.to_string() },
            }
        })
        .to_string();
        let resp = self.transport.post(GOCARDLESS_REFUNDS_URL, &headers, body).await?;
        let json: Value = serde_json::from_str(&resp.body)
            .map_err(|e| PaymentError::InvalidResponse(e.to_string()))?;
        if !resp.is_success() {
            let err = json.pointer("/error");
            return Err(PaymentError::Provider {
                code: err.and_then(|e| e.get("type")).and_then(Value::as_str).map(str::to_string),
                message: err
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("gocardless refund error")
                    .to_string(),
            });
        }
        let refund = json.pointer("/refunds").unwrap_or(&Value::Null);
        let provider_refund_id = refund
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| PaymentError::InvalidResponse("missing refunds.id".into()))?
            .to_string();
        let provider_status = refund.get("status").and_then(Value::as_str).unwrap_or("created").to_string();
        let state = match provider_status.as_str() {
            "paid" => RefundState::Succeeded,
            "cancelled" | "bounced" | "failed" | "funds_returned" => RefundState::Failed,
            _ => RefundState::Pending,
        };
        Ok(RefundOutcome { provider_refund_id, provider_status, state })
    }
}
