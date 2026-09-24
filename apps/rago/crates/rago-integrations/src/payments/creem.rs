//! Creem payment provider (merchant of record).
//!
//! Creem has no "charge a saved card" API. An invoice is collected through a
//! hosted checkout for the organization's one-time credit product
//! (`product_id`) at the invoice amount (`custom_price`, in the product's
//! currency, minimum 100 minor units). The checkout id is the provider payment
//! id, and the outcome is [`PayableStatus::Pending`] with a `redirect_to_url`
//! next action. That is the same shape as a Stripe 3-D Secure charge, so
//! `payment.requires_action` carries the pay link. The signed
//! `checkout.completed` webhook settles it.
//!
//! Refunds: `POST /v1/refunds` refunds a whole transaction, identified by
//! `transaction_id`. The transaction is resolved from the checkout's order
//! (`GET /v1/checkouts?checkout_id=` then
//! `GET /v1/transactions/search?order_id=`). Creem's API cannot refund part of
//! a payment, so a partial refund is refused with a clear provider error and
//! must be issued from the Creem dashboard. The refund response has no id, so
//! Rago records the transaction id as the provider refund id. The
//! `refund.created` webhook carries that transaction id, and it settles the
//! refund.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{
    PayableStatus, PaymentError, PaymentOutcome, PaymentProvider, PaymentRequest, RefundOutcome,
    RefundRequest, RefundState,
};
use crate::transport::HttpTransport;

const LIVE_BASE: &str = "https://api.creem.io";
const TEST_BASE: &str = "https://test-api.creem.io";

/// Creem's `custom_price` floor (minor units).
pub const MIN_CUSTOM_PRICE: i64 = 100;

/// `PaymentProviders::Creem` over an injectable transport `T`.
pub struct CreemProvider<T: HttpTransport> {
    api_key: String,
    product_id: String,
    product_currency: String,
    success_url: Option<String>,
    base: &'static str,
    transport: T,
    attempt: u32,
}

impl<T: HttpTransport> CreemProvider<T> {
    /// `product_currency` is the currency of the Creem product (`custom_price`
    /// is charged in it); invoices in any other currency are refused.
    pub fn new(
        api_key: impl Into<String>,
        product_id: impl Into<String>,
        product_currency: impl Into<String>,
        transport: T,
    ) -> Self {
        let api_key = api_key.into();
        let base = if api_key.starts_with("creem_test_") { TEST_BASE } else { LIVE_BASE };
        Self {
            api_key,
            product_id: product_id.into(),
            product_currency: product_currency.into().to_uppercase(),
            success_url: None,
            base,
            transport,
            attempt: 0,
        }
    }

    /// Force test (`true`) or live (`false`) mode regardless of the key prefix.
    pub fn with_test_mode(mut self, test: bool) -> Self {
        self.base = if test { TEST_BASE } else { LIVE_BASE };
        self
    }

    /// Where Creem returns the customer after paying.
    pub fn with_success_url(mut self, url: Option<String>) -> Self {
        self.success_url = url.filter(|u| u.starts_with("https://"));
        self
    }

    /// Attempt number for the checkout `request_id` (see
    /// [`super::idempotency_key`]).
    pub fn with_attempt(mut self, attempt: u32) -> Self {
        self.attempt = attempt;
        self
    }

    fn headers(&self) -> Vec<(String, String)> {
        vec![
            ("x-api-key".to_string(), self.api_key.clone()),
            ("Content-Type".to_string(), "application/json".to_string()),
        ]
    }

    /// Map a Creem checkout `status` to the payable status.
    pub fn determine_payment_status(status: &str) -> PayableStatus {
        match status {
            "completed" | "paid" => PayableStatus::Succeeded,
            "expired" | "failed" | "canceled" | "cancelled" => PayableStatus::Failed,
            _ => PayableStatus::Pending,
        }
    }

    async fn get_json(&self, url: &str) -> Result<Value, PaymentError> {
        let resp = self.transport.get(url, &self.headers()).await?;
        let json: Value = serde_json::from_str(&resp.body)
            .map_err(|e| PaymentError::InvalidResponse(e.to_string()))?;
        if !resp.is_success() {
            return Err(provider_error(&json, "creem lookup failed"));
        }
        Ok(json)
    }

    /// The Creem transaction that paid checkout `checkout_id`, with the amount
    /// it charged before tax (the invoice amount Rago asked for).
    pub async fn checkout_transaction(&self, checkout_id: &str) -> Result<(String, i64), PaymentError> {
        let checkout = self
            .get_json(&format!("{}/v1/checkouts?checkout_id={}", self.base, encode(checkout_id)))
            .await?;
        let order_id = ref_id(&checkout["order"]).ok_or_else(|| {
            PaymentError::InvalidResponse("creem checkout has no order (not paid)".into())
        })?;
        let search = self
            .get_json(&format!(
                "{}/v1/transactions/search?order_id={}&page_size=10",
                self.base,
                encode(&order_id)
            ))
            .await?;
        let txn = search["items"]
            .as_array()
            .and_then(|items| items.first())
            .ok_or_else(|| PaymentError::InvalidResponse("creem order has no transaction".into()))?;
        let id = txn["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| PaymentError::InvalidResponse("creem transaction has no id".into()))?
            .to_string();
        let amount = txn["amount"]
            .as_i64()
            .or_else(|| txn["amount_paid"].as_i64())
            .unwrap_or(0);
        Ok((id, amount))
    }
}

#[async_trait]
impl<T: HttpTransport> PaymentProvider for CreemProvider<T> {
    fn payment_type(&self) -> &'static str {
        "creem"
    }

    async fn create_payment(&self, req: &PaymentRequest) -> Result<PaymentOutcome, PaymentError> {
        if !req.currency.eq_ignore_ascii_case(&self.product_currency) {
            return Err(PaymentError::Provider {
                code: Some("currency_mismatch".into()),
                message: format!(
                    "the Creem product is priced in {}; this invoice is in {}",
                    self.product_currency,
                    req.currency.to_uppercase()
                ),
            });
        }
        if req.amount_cents < MIN_CUSTOM_PRICE {
            return Err(PaymentError::Provider {
                code: Some("amount_below_minimum".into()),
                message: format!("Creem checkouts start at {MIN_CUSTOM_PRICE} minor units"),
            });
        }
        let mut body = json!({
            "product_id": self.product_id,
            "request_id": super::idempotency_key(&req.payment_id, self.attempt),
            "units": 1,
            "custom_price": req.amount_cents,
            "metadata": {
                "lago_payment_id": req.payment_id.to_string(),
                "lago_invoice_id": req.invoice_id.to_string(),
                "lago_customer_id": req.customer_id.to_string(),
                "invoice_number": req.reference,
            },
        });
        if let Some(url) = &self.success_url {
            body["success_url"] = json!(url);
        }
        let url = format!("{}/v1/checkouts", self.base);
        let resp = self.transport.post(&url, &self.headers(), body.to_string()).await?;
        let json: Value = serde_json::from_str(&resp.body)
            .map_err(|e| PaymentError::InvalidResponse(e.to_string()))?;
        if !resp.is_success() {
            return Err(provider_error(&json, "creem checkout failed"));
        }
        let id = json["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or_else(|| PaymentError::InvalidResponse("missing checkout id".into()))?;
        let checkout_url = json["checkout_url"]
            .as_str()
            .filter(|s| s.starts_with("https://"))
            .ok_or_else(|| PaymentError::InvalidResponse("missing checkout_url".into()))?;
        let status = json["status"].as_str().unwrap_or("pending").to_string();
        Ok(PaymentOutcome {
            provider_payment_id: id.to_string(),
            payable_status: Self::determine_payment_status(&status),
            provider_status: status,
            next_action: Some(super::redirect_next_action(checkout_url)),
        })
    }

    async fn refund_payment(&self, req: &RefundRequest) -> Result<RefundOutcome, PaymentError> {
        let (transaction_id, charged) = self.checkout_transaction(&req.provider_payment_id).await?;
        let whole = req.amount_cents == req.total_refunded_cents
            && (charged == 0 || req.amount_cents >= charged);
        if !whole {
            return Err(PaymentError::Provider {
                code: Some("partial_refund_unsupported".into()),
                message: "Creem's API refunds whole payments only; issue a partial refund from \
                          the Creem dashboard"
                    .into(),
            });
        }
        let mut headers = self.headers();
        headers.push(("Idempotency-Key".to_string(), super::refund_idempotency_key(&req.refund_id)));
        let body = json!({ "transaction_id": transaction_id }).to_string();
        let resp = self
            .transport
            .post(&format!("{}/v1/refunds", self.base), &headers, body)
            .await?;
        let json: Value = serde_json::from_str(&resp.body)
            .map_err(|e| PaymentError::InvalidResponse(e.to_string()))?;
        if !resp.is_success() {
            return Err(provider_error(&json, "creem refund failed"));
        }
        let status = json["status"].as_str().unwrap_or("pending").to_string();
        Ok(RefundOutcome {
            provider_refund_id: transaction_id,
            state: refund_state(&status),
            provider_status: status,
        })
    }
}

/// Map a Creem refund status.
pub fn refund_state(status: &str) -> RefundState {
    match status {
        "succeeded" => RefundState::Succeeded,
        "failed" | "canceled" | "cancelled" => RefundState::Failed,
        _ => RefundState::Pending,
    }
}

fn provider_error(json: &Value, fallback: &str) -> PaymentError {
    let message = json["message"]
        .as_str()
        .map(str::to_string)
        .or_else(|| json["message"].as_array().map(|m| {
            m.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ")
        }))
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| fallback.to_string());
    PaymentError::Provider {
        code: json["error"].as_str().or_else(|| json["code"].as_str()).map(str::to_string),
        message,
    }
}

/// An expanded (`{"id": ..}`) or bare-string Creem reference.
pub(crate) fn ref_id(v: &Value) -> Option<String> {
    v["id"]
        .as_str()
        .or_else(|| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// Percent-encode a query value.
pub(crate) fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{HttpResponse, MockTransport};
    use uuid::Uuid;

    fn req(amount: i64, currency: &str) -> PaymentRequest {
        PaymentRequest::new(
            Uuid::nil(),
            Uuid::from_u128(1),
            Uuid::from_u128(2),
            "cust_ext",
            amount,
            currency,
            "INV-001",
        )
    }

    #[tokio::test]
    async fn checkout_is_created_at_the_invoice_amount_and_returns_a_pay_link() {
        let t = MockTransport::always(
            200,
            r#"{"id":"ch_1","status":"pending","checkout_url":"https://creem.io/checkout/ch_1"}"#,
        );
        let p = CreemProvider::new("creem_test_k", "prod_1", "usd", &t)
            .with_success_url(Some("https://app.test/paid".into()));
        let out = p.create_payment(&req(2_599, "USD")).await.unwrap();
        assert_eq!(out.provider_payment_id, "ch_1");
        assert_eq!(out.payable_status, PayableStatus::Pending);
        assert_eq!(
            crate::payments::stripe::next_action_redirect_url(out.next_action.as_ref().unwrap()),
            Some("https://creem.io/checkout/ch_1".into())
        );
        let sent = t.last_request().unwrap();
        assert_eq!(sent.url, "https://test-api.creem.io/v1/checkouts");
        assert!(sent.headers.iter().any(|(k, v)| k == "x-api-key" && v == "creem_test_k"));
        let body: Value = serde_json::from_str(&sent.body).unwrap();
        assert_eq!(body["custom_price"], 2_599);
        assert_eq!(body["product_id"], "prod_1");
        assert_eq!(body["request_id"], format!("payment-{}", Uuid::nil()));
        assert_eq!(body["metadata"]["lago_invoice_id"], Uuid::from_u128(1).to_string());
        assert_eq!(body["success_url"], "https://app.test/paid");
    }

    #[tokio::test]
    async fn currency_mismatch_and_tiny_amounts_are_declined_without_a_call() {
        let t = MockTransport::always(500, "{}");
        let p = CreemProvider::new("k", "prod_1", "USD", &t);
        assert!(matches!(
            p.create_payment(&req(1_000, "EUR")).await,
            Err(PaymentError::Provider { code: Some(c), .. }) if c == "currency_mismatch"
        ));
        assert!(matches!(
            p.create_payment(&req(50, "USD")).await,
            Err(PaymentError::Provider { code: Some(c), .. }) if c == "amount_below_minimum"
        ));
        assert!(t.requests().is_empty());
    }

    fn refund(amount: i64, total: i64) -> RefundRequest {
        RefundRequest {
            refund_id: Uuid::from_u128(9),
            provider_payment_id: "ch_1".into(),
            amount_cents: amount,
            currency: "USD".into(),
            total_refunded_cents: total,
            reason: None,
            reference: "CN-1".into(),
        }
    }

    #[tokio::test]
    async fn full_refund_resolves_the_transaction_and_records_it() {
        let ok = |body: &str| Ok(HttpResponse { status: 200, body: body.to_string() });
        let t = MockTransport::scripted(
            vec![
                ok(r#"{"id":"ch_1","status":"completed","order":{"id":"ord_1"}}"#),
                ok(r#"{"items":[{"id":"tran_1","amount":2599,"amount_paid":3145}]}"#),
                ok(r#"{"status":"pending"}"#),
            ],
            HttpResponse { status: 500, body: "{}".into() },
        );
        let p = CreemProvider::new("k", "prod_1", "USD", &t);
        let out = p.refund_payment(&refund(2_599, 2_599)).await.unwrap();
        assert_eq!(out.provider_refund_id, "tran_1");
        assert_eq!(out.state, RefundState::Pending);
        let sent = t.requests();
        assert_eq!(sent[0].url, "https://api.creem.io/v1/checkouts?checkout_id=ch_1");
        assert!(sent[1].url.contains("/v1/transactions/search?order_id=ord_1"));
        assert_eq!(sent[2].url, "https://api.creem.io/v1/refunds");
        assert_eq!(sent[2].body, r#"{"transaction_id":"tran_1"}"#);
        assert!(sent[2]
            .headers
            .iter()
            .any(|(k, v)| k == "Idempotency-Key" && v == &format!("refund-{}", Uuid::from_u128(9))));
    }

    #[tokio::test]
    async fn partial_refunds_are_refused_before_calling_refunds() {
        let ok = |body: &str| Ok(HttpResponse { status: 200, body: body.to_string() });
        let t = MockTransport::scripted(
            vec![
                ok(r#"{"id":"ch_1","order":"ord_1"}"#),
                ok(r#"{"items":[{"id":"tran_1","amount":2599}]}"#),
            ],
            HttpResponse { status: 500, body: "{}".into() },
        );
        let p = CreemProvider::new("k", "prod_1", "USD", &t);
        let err = p.refund_payment(&refund(1_000, 1_000)).await.unwrap_err();
        assert!(matches!(err, PaymentError::Provider { code: Some(c), .. } if c == "partial_refund_unsupported"));
        assert_eq!(t.requests().len(), 2);
    }
}
