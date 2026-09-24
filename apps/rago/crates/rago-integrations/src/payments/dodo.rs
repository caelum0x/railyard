//! Dodo Payments provider (merchant of record).
//!
//! Like Creem, Dodo collects through a hosted checkout. A checkout session is
//! created for the organization's pay-what-you-want product (`product_id`), with
//! one unit whose `amount` is the invoice total, billed in the invoice currency
//! (`billing_currency`). The result is [`PayableStatus::Pending`] with a
//! `redirect_to_url` next action. When Dodo already returns a `payment_id`,
//! that is the provider payment id. Otherwise the session id is used and the
//! `payment.succeeded` webhook rebinds the payment to the real `payment_id`
//! through `checkout_session_id`.
//!
//! Refunds: `POST /refunds` with the payment id. The refund is issued as a
//! line refund of the product line for the requested amount, so partial
//! refunds work. `metadata.lago_refund_id` makes it traceable. The final state
//! arrives as `refund.succeeded` / `refund.failed`.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{
    PayableStatus, PaymentError, PaymentOutcome, PaymentProvider, PaymentRequest, RefundOutcome,
    RefundRequest, RefundState,
};
use crate::transport::HttpTransport;

const LIVE_BASE: &str = "https://live.dodopayments.com";
const TEST_BASE: &str = "https://test.dodopayments.com";

/// `PaymentProviders::Dodo` over an injectable transport `T`.
pub struct DodoProvider<T: HttpTransport> {
    api_key: String,
    product_id: String,
    return_url: Option<String>,
    base: &'static str,
    transport: T,
}

impl<T: HttpTransport> DodoProvider<T> {
    /// `test_mode` selects `test.dodopayments.com`.
    pub fn new(
        api_key: impl Into<String>,
        product_id: impl Into<String>,
        test_mode: bool,
        transport: T,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            product_id: product_id.into(),
            return_url: None,
            base: if test_mode { TEST_BASE } else { LIVE_BASE },
            transport,
        }
    }

    /// Where Dodo returns the customer after paying.
    pub fn with_return_url(mut self, url: Option<String>) -> Self {
        self.return_url = url.filter(|u| u.starts_with("https://"));
        self
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![
            ("Authorization".to_string(), format!("Bearer {}", self.api_key)),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]
    }

    /// Map a Dodo payment `status`.
    pub fn determine_payment_status(status: &str) -> PayableStatus {
        match status {
            "succeeded" => PayableStatus::Succeeded,
            "failed" | "cancelled" => PayableStatus::Failed,
            _ => PayableStatus::Pending,
        }
    }
}

#[async_trait]
impl<T: HttpTransport> PaymentProvider for DodoProvider<T> {
    fn payment_type(&self) -> &'static str {
        "dodo"
    }

    async fn create_payment(&self, req: &PaymentRequest) -> Result<PaymentOutcome, PaymentError> {
        if req.amount_cents <= 0 {
            return Err(PaymentError::Provider {
                code: Some("nothing_to_collect".into()),
                message: "Dodo checkouts need a positive amount".into(),
            });
        }
        let mut body = json!({
            "product_cart": [{
                "product_id": self.product_id,
                "quantity": 1,
                "amount": req.amount_cents,
            }],
            "billing_currency": req.currency.to_uppercase(),
            "metadata": {
                "lago_payment_id": req.payment_id.to_string(),
                "lago_invoice_id": req.invoice_id.to_string(),
                "lago_customer_id": req.customer_id.to_string(),
                "invoice_number": req.reference,
            },
        });
        if req.provider_customer_id.starts_with("cus_") {
            body["customer"] = json!({ "customer_id": req.provider_customer_id });
        }
        if let Some(url) = &self.return_url {
            body["return_url"] = json!(url);
        }
        let resp = self
            .transport
            .post(&format!("{}/checkouts", self.base), &self.headers(), body.to_string())
            .await?;
        let json: Value = serde_json::from_str(&resp.body)
            .map_err(|e| PaymentError::InvalidResponse(e.to_string()))?;
        if !resp.is_success() {
            return Err(provider_error(&json, "dodo checkout failed"));
        }
        let session_id = json["session_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| PaymentError::InvalidResponse("missing session_id".into()))?;
        let checkout_url = json["checkout_url"]
            .as_str()
            .filter(|s| s.starts_with("https://"))
            .ok_or_else(|| PaymentError::InvalidResponse("missing checkout_url".into()))?;
        let provider_payment_id = json["payment_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(session_id)
            .to_string();
        Ok(PaymentOutcome {
            provider_payment_id,
            provider_status: "requires_customer_action".into(),
            payable_status: PayableStatus::Pending,
            next_action: Some(super::redirect_next_action(checkout_url)),
        })
    }

    async fn refund_payment(&self, req: &RefundRequest) -> Result<RefundOutcome, PaymentError> {
        if !req.provider_payment_id.starts_with("pay_") {
            return Err(PaymentError::Provider {
                code: Some("payment_not_settled".into()),
                message: "this Dodo checkout has no settled payment to refund yet".into(),
            });
        }
        let mut body = json!({
            "payment_id": req.provider_payment_id,
            "items": [{ "item_id": self.product_id, "amount": req.amount_cents }],
            "metadata": {
                "lago_refund_id": req.refund_id.to_string(),
                "reference": req.reference,
            },
        });
        if let Some(reason) = &req.reason {
            body["reason"] = json!(reason);
        }
        let mut headers = self.headers();
        headers.push(("Idempotency-Key".to_string(), super::refund_idempotency_key(&req.refund_id)));
        let resp = self
            .transport
            .post(&format!("{}/refunds", self.base), &headers, body.to_string())
            .await?;
        let json: Value = serde_json::from_str(&resp.body)
            .map_err(|e| PaymentError::InvalidResponse(e.to_string()))?;
        if !resp.is_success() {
            return Err(provider_error(&json, "dodo refund failed"));
        }
        let provider_refund_id = json["refund_id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| PaymentError::InvalidResponse("missing refund_id".into()))?
            .to_string();
        let status = json["status"].as_str().unwrap_or("pending").to_string();
        Ok(RefundOutcome { provider_refund_id, state: refund_state(&status), provider_status: status })
    }
}

/// Map a Dodo refund status (`review` is still in flight).
pub fn refund_state(status: &str) -> RefundState {
    match status {
        "succeeded" => RefundState::Succeeded,
        "failed" => RefundState::Failed,
        _ => RefundState::Pending,
    }
}

fn provider_error(json: &Value, fallback: &str) -> PaymentError {
    PaymentError::Provider {
        code: json["code"].as_str().map(str::to_string),
        message: json["message"].as_str().filter(|m| !m.is_empty()).unwrap_or(fallback).to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::MockTransport;
    use uuid::Uuid;

    #[tokio::test]
    async fn checkout_session_prices_the_line_at_the_invoice_amount() {
        let t = MockTransport::always(
            200,
            r#"{"session_id":"cks_1","checkout_url":"https://test.checkout.dodopayments.com/cks_1","payment_id":null}"#,
        );
        let p = DodoProvider::new("dodo_k", "pdt_1", true, &t)
            .with_return_url(Some("https://app.test/done".into()));
        let req = PaymentRequest::new(
            Uuid::nil(),
            Uuid::from_u128(1),
            Uuid::from_u128(2),
            "cus_9",
            4_200,
            "try",
            "INV-7",
        );
        let out = p.create_payment(&req).await.unwrap();
        assert_eq!(out.provider_payment_id, "cks_1");
        assert_eq!(out.payable_status, PayableStatus::Pending);
        let sent = t.last_request().unwrap();
        assert_eq!(sent.url, "https://test.dodopayments.com/checkouts");
        assert!(sent.headers.iter().any(|(k, v)| k == "Authorization" && v == "Bearer dodo_k"));
        let body: Value = serde_json::from_str(&sent.body).unwrap();
        assert_eq!(body["product_cart"][0]["amount"], 4_200);
        assert_eq!(body["billing_currency"], "TRY");
        assert_eq!(body["customer"]["customer_id"], "cus_9");
        assert_eq!(body["return_url"], "https://app.test/done");
        assert_eq!(body["metadata"]["lago_payment_id"], Uuid::nil().to_string());
    }

    #[tokio::test]
    async fn refund_targets_the_product_line_with_the_requested_amount() {
        let t = MockTransport::always(200, r#"{"refund_id":"ref_1","status":"review","amount":500}"#);
        let p = DodoProvider::new("dodo_k", "pdt_1", false, &t);
        let out = p
            .refund_payment(&RefundRequest {
                refund_id: Uuid::from_u128(3),
                provider_payment_id: "pay_1".into(),
                amount_cents: 500,
                currency: "USD".into(),
                total_refunded_cents: 500,
                reason: Some("order_change".into()),
                reference: "CN-2".into(),
            })
            .await
            .unwrap();
        assert_eq!(out.provider_refund_id, "ref_1");
        assert_eq!(out.state, RefundState::Pending);
        let sent = t.last_request().unwrap();
        assert_eq!(sent.url, "https://live.dodopayments.com/refunds");
        let body: Value = serde_json::from_str(&sent.body).unwrap();
        assert_eq!(body["payment_id"], "pay_1");
        assert_eq!(body["items"][0], json!({"item_id":"pdt_1","amount":500}));
        assert_eq!(body["reason"], "order_change");
    }

    #[tokio::test]
    async fn refund_of_an_unsettled_session_is_refused() {
        let t = MockTransport::always(500, "{}");
        let p = DodoProvider::new("k", "pdt_1", false, &t);
        let err = p
            .refund_payment(&RefundRequest {
                refund_id: Uuid::nil(),
                provider_payment_id: "cks_1".into(),
                amount_cents: 1,
                currency: "USD".into(),
                total_refunded_cents: 1,
                reason: None,
                reference: "x".into(),
            })
            .await
            .unwrap_err();
        assert!(matches!(err, PaymentError::Provider { .. }));
        assert!(t.requests().is_empty());
    }
}
