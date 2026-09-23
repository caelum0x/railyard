//! Adyen payment provider — faithful port of
//! `PaymentProviders::Adyen::Payments::CreateService` + `AdyenProvider`.
//!
//! Creates a Checkout payment (`POST /v71/payments`, `X-API-Key` header) with a
//! recurring subscription flow, and maps the `resultCode`:
//! * `AuthorisedPending` / `Received` → [`PayableStatus::Pending`]
//! * `Authorised` / `SentForSettle` / `SettleScheduled` / `Settled` / `Refunded`
//!   → [`PayableStatus::Succeeded`]
//! * `Cancelled` / `CaptureFailed` / `Error` / `Expired` / `Refused` →
//!   [`PayableStatus::Failed`]

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{
    PaymentError, PaymentOutcome, PaymentProvider, PaymentRequest, PayableStatus, RefundOutcome,
    RefundRequest, RefundState,
};
use crate::transport::HttpTransport;

const ADYEN_PAYMENTS_URL: &str = "https://checkout-test.adyen.com/v71/payments";
const ADYEN_CHECKOUT_BASE: &str = "https://checkout-test.adyen.com/v71";
const PROCESSING_STATUSES: &[&str] = &["AuthorisedPending", "Received"];
const SUCCESS_STATUSES: &[&str] =
    &["Authorised", "SentForSettle", "SettleScheduled", "Settled", "Refunded"];
const FAILED_STATUSES: &[&str] = &["Cancelled", "CaptureFailed", "Error", "Expired", "Refused"];

/// `PaymentProviders::Adyen` over an injectable transport `T`.
pub struct AdyenProvider<T: HttpTransport> {
    api_key: String,
    merchant_account: String,
    transport: T,
    attempt: u32,
}

impl<T: HttpTransport> AdyenProvider<T> {
    pub fn new(
        api_key: impl Into<String>,
        merchant_account: impl Into<String>,
        transport: T,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            merchant_account: merchant_account.into(),
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

    /// `AdyenProvider#determine_payment_status`.
    pub fn determine_payment_status(status: &str) -> PayableStatus {
        if PROCESSING_STATUSES.contains(&status) {
            PayableStatus::Pending
        } else if SUCCESS_STATUSES.contains(&status) {
            PayableStatus::Succeeded
        } else if FAILED_STATUSES.contains(&status) {
            PayableStatus::Failed
        } else {
            PayableStatus::Pending
        }
    }
}

#[async_trait]
impl<T: HttpTransport> PaymentProvider for AdyenProvider<T> {
    fn payment_type(&self) -> &'static str {
        "adyen"
    }

    async fn create_payment(&self, req: &PaymentRequest) -> Result<PaymentOutcome, PaymentError> {
        let headers = vec![
            ("X-API-Key".to_string(), self.api_key.clone()),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Idempotency-Key".to_string(), super::idempotency_key(&req.payment_id, self.attempt)),
        ];
        let body = json!({
            "amount": { "value": req.amount_cents, "currency": req.currency.to_uppercase() },
            "reference": req.reference,
            "shopperReference": req.provider_customer_id,
            "merchantAccount": self.merchant_account,
            "shopperInteraction": "ContAuth",
            "recurringProcessingModel": "Subscription",
            "metadata": {
                "lago_payment_id": req.payment_id.to_string(),
                "lago_invoice_id": req.invoice_id.to_string(),
            },
        })
        .to_string();

        let resp = self.transport.post(ADYEN_PAYMENTS_URL, &headers, body).await?;
        let json: Value = serde_json::from_str(&resp.body)
            .map_err(|e| PaymentError::InvalidResponse(e.to_string()))?;

        if !resp.is_success() {
            return Err(PaymentError::Provider {
                code: json.get("errorCode").and_then(Value::as_str).map(str::to_string),
                message: json
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("adyen error")
                    .to_string(),
            });
        }

        let result_code = json
            .get("resultCode")
            .and_then(Value::as_str)
            .ok_or_else(|| PaymentError::InvalidResponse("missing resultCode".into()))?
            .to_string();
        let psp_reference = json
            .get("pspReference")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();

        Ok(PaymentOutcome {
            payable_status: Self::determine_payment_status(&result_code),
            provider_payment_id: psp_reference,
            provider_status: result_code,
            next_action: None,
        })
    }

    /// `PaymentProviders::Adyen::Refunds::CreateService` — `POST
    /// /payments/{pspReference}/refunds`. Adyen answers `received`; the final
    /// outcome arrives asynchronously as a `REFUND` notification.
    async fn refund_payment(&self, req: &RefundRequest) -> Result<RefundOutcome, PaymentError> {
        let headers = vec![
            ("X-API-Key".to_string(), self.api_key.clone()),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Idempotency-Key".to_string(), super::refund_idempotency_key(&req.refund_id)),
        ];
        let body = json!({
            "amount": { "value": req.amount_cents, "currency": req.currency.to_uppercase() },
            "merchantAccount": self.merchant_account,
            "reference": req.reference,
        })
        .to_string();
        let url = format!("{ADYEN_CHECKOUT_BASE}/payments/{}/refunds", req.provider_payment_id);
        let resp = self.transport.post(&url, &headers, body).await?;
        let json: Value = serde_json::from_str(&resp.body)
            .map_err(|e| PaymentError::InvalidResponse(e.to_string()))?;
        if !resp.is_success() {
            return Err(PaymentError::Provider {
                code: json.get("errorCode").and_then(Value::as_str).map(str::to_string),
                message: json
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("adyen refund error")
                    .to_string(),
            });
        }
        let provider_refund_id = json
            .get("pspReference")
            .and_then(Value::as_str)
            .ok_or_else(|| PaymentError::InvalidResponse("missing pspReference".into()))?
            .to_string();
        let provider_status = json.get("status").and_then(Value::as_str).unwrap_or("received").to_string();
        Ok(RefundOutcome { provider_refund_id, provider_status, state: RefundState::Pending })
    }
}
