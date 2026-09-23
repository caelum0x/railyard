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

use super::{
    PaymentError, PaymentOutcome, PaymentProvider, PaymentRequest, PayableStatus, RefundOutcome,
    RefundRequest, RefundState,
};
use crate::transport::HttpTransport;

const STRIPE_PAYMENT_INTENTS_URL: &str = "https://api.stripe.com/v1/payment_intents";
const SUCCESS_STATUSES: &[&str] = &["succeeded"];
const FAILED_STATUSES: &[&str] = &["canceled", "requires_payment_method"];

const STRIPE_API: &str = "https://api.stripe.com/v1";

/// `PaymentProviders::Stripe` over an injectable transport `T`.
pub struct StripeProvider<T: HttpTransport> {
    secret_key: String,
    transport: T,
    /// The stored Stripe `pm_…` to confirm the PaymentIntent with (Lago passes
    /// the customer's saved `payment_method_id`; without one Stripe rejects an
    /// off-session confirm).
    payment_method_id: Option<String>,
    /// Attempt discriminator appended to the idempotency key. Stripe caches a
    /// 4xx (e.g. `card_declined`) under its key for 24h, so every legitimate
    /// retry of a FAILED charge needs its own key — while a replay of the same
    /// attempt reuses it and is deduplicated.
    attempt: u32,
}

impl<T: HttpTransport> StripeProvider<T> {
    pub fn new(secret_key: impl Into<String>, transport: T) -> Self {
        Self {
            secret_key: secret_key.into(),
            transport,
            payment_method_id: None,
            attempt: 0,
        }
    }

    /// Confirm charges with this saved payment method.
    pub fn with_payment_method(mut self, payment_method_id: Option<String>) -> Self {
        self.payment_method_id = payment_method_id.filter(|p| !p.is_empty());
        self
    }

    /// Use attempt `n` of this payment in the idempotency key (`0` = first).
    pub fn with_attempt(mut self, attempt: u32) -> Self {
        self.attempt = attempt;
        self
    }

    /// The idempotency key for a payment attempt: `payment-<id>` for the first
    /// attempt (unchanged from the original key), `payment-<id>-<n>` after.
    pub fn idempotency_key(payment_id: &uuid::Uuid, attempt: u32) -> String {
        super::idempotency_key(payment_id, attempt)
    }

    fn auth_headers(&self, idempotency_key: Option<String>) -> Vec<(String, String)> {
        let mut h = vec![
            ("Authorization".to_string(), format!("Bearer {}", self.secret_key)),
            (
                "Content-Type".to_string(),
                "application/x-www-form-urlencoded".to_string(),
            ),
        ];
        if let Some(k) = idempotency_key {
            h.push(("Idempotency-Key".to_string(), k));
        }
        h
    }

    async fn post_form(
        &self,
        url: &str,
        pairs: &[(String, String)],
        idempotency_key: Option<String>,
    ) -> Result<Value, PaymentError> {
        let resp = self
            .transport
            .post(url, &self.auth_headers(idempotency_key), form_urlencode(pairs))
            .await?;
        parse_stripe_response(resp.status, &resp.body)
    }

    async fn get_json(&self, url: &str) -> Result<Value, PaymentError> {
        let resp = self.transport.get(url, &self.auth_headers(None)).await?;
        parse_stripe_response(resp.status, &resp.body)
    }

    /// `PaymentProviders::Stripe::Customers::CreateService` — create the Stripe
    /// customer for a Lago customer (idempotent per Lago customer id). Returns
    /// the `cus_…` id.
    pub async fn create_customer(
        &self,
        lago_customer_id: &uuid::Uuid,
        name: Option<&str>,
        email: Option<&str>,
    ) -> Result<String, PaymentError> {
        let mut pairs = vec![(
            "metadata[lago_customer_id]".to_string(),
            lago_customer_id.to_string(),
        )];
        if let Some(n) = name.filter(|n| !n.is_empty()) {
            pairs.push(("name".into(), n.to_string()));
        }
        if let Some(e) = email.filter(|e| e.contains('@')) {
            pairs.push(("email".into(), e.to_string()));
        }
        let json = self
            .post_form(
                &format!("{STRIPE_API}/customers"),
                &pairs,
                Some(format!("customer-{lago_customer_id}")),
            )
            .await?;
        json.get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| PaymentError::InvalidResponse("missing customer id".into()))
    }

    /// Resolve the payment method to charge for `customer`: the customer's
    /// `invoice_settings.default_payment_method`, else their first saved
    /// payment method. `None` when the customer has none on file.
    pub async fn default_payment_method(
        &self,
        customer: &str,
    ) -> Result<Option<String>, PaymentError> {
        let cus = self
            .get_json(&format!("{STRIPE_API}/customers/{}", percent_encode(customer)))
            .await?;
        let default = cus
            .pointer("/invoice_settings/default_payment_method")
            .and_then(|v| v.as_str().map(str::to_string).or_else(|| v.get("id").and_then(Value::as_str).map(str::to_string)));
        if default.is_some() {
            return Ok(default);
        }
        let list = self
            .get_json(&format!(
                "{STRIPE_API}/customers/{}/payment_methods?limit=1",
                percent_encode(customer)
            ))
            .await?;
        Ok(list
            .pointer("/data/0/id")
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    /// `PaymentProviders::Stripe::Customers::GenerateCheckoutUrlService` — a
    /// Checkout Session in `setup` mode where the customer saves a card.
    pub async fn create_checkout_session(
        &self,
        customer: &str,
        success_url: &str,
    ) -> Result<String, PaymentError> {
        let pairs = vec![
            ("customer".to_string(), customer.to_string()),
            ("mode".to_string(), "setup".to_string()),
            ("payment_method_types[0]".to_string(), "card".to_string()),
            ("success_url".to_string(), success_url.to_string()),
        ];
        let json = self
            .post_form(&format!("{STRIPE_API}/checkout/sessions"), &pairs, None)
            .await?;
        json.get("url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| PaymentError::InvalidResponse("missing checkout url".into()))
    }

    /// `PaymentProviders::Stripe::Payments::GeneratePaymentUrlService` — a
    /// one-time Checkout Session (payment mode) for an invoice's amount.
    pub async fn create_payment_checkout(
        &self,
        customer: &str,
        amount_cents: i64,
        currency: &str,
        invoice_id: &uuid::Uuid,
        invoice_number: &str,
        success_url: &str,
    ) -> Result<String, PaymentError> {
        let pairs = vec![
            ("customer".to_string(), customer.to_string()),
            ("mode".to_string(), "payment".to_string()),
            ("success_url".to_string(), success_url.to_string()),
            ("line_items[0][quantity]".to_string(), "1".to_string()),
            ("line_items[0][price_data][currency]".to_string(), currency.to_lowercase()),
            ("line_items[0][price_data][unit_amount]".to_string(), amount_cents.to_string()),
            ("line_items[0][price_data][product_data][name]".to_string(), invoice_number.to_string()),
            ("payment_intent_data[metadata][lago_payable_id]".to_string(), invoice_id.to_string()),
            ("payment_intent_data[metadata][lago_payable_type]".to_string(), "Invoice".to_string()),
            ("payment_intent_data[metadata][lago_invoice_id]".to_string(), invoice_id.to_string()),
            ("payment_intent_data[setup_future_usage]".to_string(), "off_session".to_string()),
        ];
        let json = self
            .post_form(&format!("{STRIPE_API}/checkout/sessions"), &pairs, None)
            .await?;
        json.get("url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| PaymentError::InvalidResponse("missing checkout url".into()))
    }

    /// A Stripe Billing Portal session where the customer manages payment
    /// methods and sees their payment history.
    pub async fn create_portal_session(
        &self,
        customer: &str,
        return_url: &str,
    ) -> Result<String, PaymentError> {
        let pairs = vec![
            ("customer".to_string(), customer.to_string()),
            ("return_url".to_string(), return_url.to_string()),
        ];
        let json = self
            .post_form(&format!("{STRIPE_API}/billing_portal/sessions"), &pairs, None)
            .await?;
        json.get("url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| PaymentError::InvalidResponse("missing portal url".into()))
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
    fn payment_intent_form(req: &PaymentRequest, payment_method_id: Option<&str>) -> String {
        let mut pairs: Vec<(String, String)> = vec![
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
        if let Some(pm) = payment_method_id {
            pairs.push(("payment_method".into(), pm.to_string()));
            pairs.push(("payment_method_types[0]".into(), "card".into()));
        }
        form_urlencode(&pairs)
    }
}

#[async_trait]
impl<T: HttpTransport> PaymentProvider for StripeProvider<T> {
    fn payment_type(&self) -> &'static str {
        "stripe"
    }

    async fn create_payment(&self, req: &PaymentRequest) -> Result<PaymentOutcome, PaymentError> {
        let headers =
            self.auth_headers(Some(Self::idempotency_key(&req.payment_id, self.attempt)));
        let body = Self::payment_intent_form(req, self.payment_method_id.as_deref());

        let resp = self
            .transport
            .post(STRIPE_PAYMENT_INTENTS_URL, &headers, body)
            .await?;

        let json: Value = serde_json::from_str(&resp.body)
            .map_err(|e| PaymentError::InvalidResponse(e.to_string()))?;

        if !resp.is_success() {
            // A card decline is a normal business outcome, not an outage: Stripe
            // returns 402 with the (failed) PaymentIntent embedded. Surface it as
            // a Failed outcome so the invoice is marked failed and dunning runs.
            let err = json.get("error");
            let decline_code = err
                .and_then(|e| e.get("decline_code").or_else(|| e.get("code")))
                .and_then(Value::as_str)
                .unwrap_or("card_declined")
                .to_string();
            let is_card_error = err
                .and_then(|e| e.get("type"))
                .and_then(Value::as_str)
                == Some("card_error");
            // Off-session SCA: the bank wants the cardholder to authenticate.
            // Not a decline — the customer completes it through a confirm link.
            if decline_code == "authentication_required" {
                let pi_id = err
                    .and_then(|e| e.pointer("/payment_intent/id"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("auth-required-{}-{}", req.payment_id, self.attempt));
                let next_action = err
                    .and_then(|e| e.pointer("/payment_intent/next_action"))
                    .filter(|v| !v.is_null())
                    .cloned()
                    .unwrap_or_else(|| serde_json::json!({ "type": "authentication_required" }));
                return Ok(PaymentOutcome {
                    payable_status: PayableStatus::Pending,
                    provider_payment_id: pi_id,
                    provider_status: "requires_action".into(),
                    next_action: Some(next_action),
                });
            }
            if is_card_error || resp.status == 402 {
                let pi_id = err
                    .and_then(|e| e.pointer("/payment_intent/id"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("declined-{}-{}", req.payment_id, self.attempt));
                return Ok(PaymentOutcome {
                    payable_status: PayableStatus::Failed,
                    provider_payment_id: pi_id,
                    provider_status: decline_code,
                    next_action: None,
                });
            }
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

        // `requires_action` (SCA / 3-D Secure): keep what Stripe asks the
        // customer to do so a confirm link can be sent.
        let next_action = (provider_status == "requires_action")
            .then(|| json.get("next_action").cloned().filter(|v| !v.is_null()))
            .flatten()
            .or_else(|| {
                (provider_status == "requires_action")
                    .then(|| serde_json::json!({ "type": "use_stripe_sdk" }))
            });

        Ok(PaymentOutcome {
            payable_status: Self::determine_payment_status(&provider_status),
            provider_payment_id,
            provider_status,
            next_action,
        })
    }

    /// `PaymentProviders::Stripe::Refunds::CreateService` — `POST /v1/refunds`
    /// against the PaymentIntent, idempotent per Rago refund id.
    async fn refund_payment(&self, req: &RefundRequest) -> Result<RefundOutcome, PaymentError> {
        let mut pairs = vec![
            ("payment_intent".to_string(), req.provider_payment_id.clone()),
            ("amount".to_string(), req.amount_cents.to_string()),
            ("metadata[lago_refund_id]".to_string(), req.refund_id.to_string()),
            ("metadata[reference]".to_string(), req.reference.clone()),
        ];
        pairs.push(("reason".to_string(), stripe_refund_reason(req.reason.as_deref()).to_string()));
        let json = self
            .post_form(
                &format!("{STRIPE_API}/refunds"),
                &pairs,
                Some(super::refund_idempotency_key(&req.refund_id)),
            )
            .await?;
        let provider_refund_id = json
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| PaymentError::InvalidResponse("missing refund id".into()))?
            .to_string();
        let provider_status = json
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("pending")
            .to_string();
        Ok(RefundOutcome {
            state: stripe_refund_state(&provider_status),
            provider_refund_id,
            provider_status,
        })
    }
}

/// Map a Lago credit-note reason onto Stripe's refund `reason` enum.
fn stripe_refund_reason(reason: Option<&str>) -> &'static str {
    match reason {
        Some("duplicated_charge") => "duplicate",
        Some("fraudulent_charge") => "fraudulent",
        _ => "requested_by_customer",
    }
}

/// Stripe refund status → [`RefundState`].
pub fn stripe_refund_state(status: &str) -> RefundState {
    match status {
        "succeeded" => RefundState::Succeeded,
        "failed" | "canceled" => RefundState::Failed,
        _ => RefundState::Pending,
    }
}

/// Decode a Stripe JSON response, mapping non-2xx to [`PaymentError::Provider`].
fn parse_stripe_response(status: u16, body: &str) -> Result<Value, PaymentError> {
    let json: Value =
        serde_json::from_str(body).map_err(|e| PaymentError::InvalidResponse(e.to_string()))?;
    if !(200..300).contains(&status) {
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
    Ok(json)
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

/// The customer-facing URL of a Stripe `next_action`, when Stripe hosts one
/// (`redirect_to_url`). `use_stripe_sdk` actions have none: the customer is
/// sent a hosted Checkout link instead.
pub fn next_action_redirect_url(next_action: &Value) -> Option<String> {
    next_action
        .pointer("/redirect_to_url/url")
        .and_then(Value::as_str)
        .filter(|u| u.starts_with("https://"))
        .map(str::to_string)
}
