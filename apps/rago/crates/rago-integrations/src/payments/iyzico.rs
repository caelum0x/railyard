//! Iyzico payment provider (Turkish card acquirer), ported from Mint's
//! `mint-payments/src/crypto_tr/iyzico.rs`.
//!
//! **Collect**: an IYZWSv2-signed hosted Checkout Form
//! (`/payment/iyzipos/checkoutform/initialize/auth/ecom`). The form `token` is
//! the provider payment id until the payment settles. `conversationId` is the
//! Rago payment id and `basketId` is the invoice id. Iyzico returns the buyer's
//! browser to `callbackUrl` (Rago's `/webhooks/iyzico/:organization_id`) with
//! the `token`. The token is resolved through the signed checkout-form detail
//! call, and the payment is rebound to Iyzico's numeric `paymentId`. Merchant
//! notifications (`X-IYZ-SIGNATURE-V3`) settle it server to server as well.
//!
//! **Refund**: `/payment/refund` against the payment's single item transaction
//! (`paymentTransactionId`, looked up from `/payment/detail`). Iyzico answers
//! synchronously, so a `success` response means the refund succeeded.
//!
//! Buyer data: Iyzico requires a buyer and a billing address on every form.
//! Rago sends the customer's own record: name, email, country, street address,
//! city, postal code, phone and identity number (TC kimlik no for Turkish
//! buyers). Nothing is filled with placeholders: a customer missing any
//! required field is refused before Iyzico is called, with an error naming the
//! missing fields so the operator can complete the customer.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;

use super::{
    PayableStatus, PaymentError, PaymentOutcome, PaymentProvider, PaymentRequest, RefundOutcome,
    RefundRequest, RefundState,
};
use crate::transport::HttpTransport;

const LIVE_BASE: &str = "https://api.iyzipay.com";
const SANDBOX_BASE: &str = "https://sandbox-api.iyzipay.com";

/// Currencies Iyzico settles.
pub const SUPPORTED_CURRENCIES: &[&str] = &["TRY", "USD", "EUR", "GBP", "NOK", "CHF", "RUB", "IRR"];


static NONCE: AtomicU64 = AtomicU64::new(0);

/// Buyer + billing details Iyzico requires on every checkout form.
#[derive(Debug, Clone, Default)]
pub struct IyzicoBuyer {
    pub id: String,
    pub name: String,
    pub email: Option<String>,
    pub country: String,
    pub city: String,
    pub address: String,
    pub zip_code: Option<String>,
    pub phone: Option<String>,
    /// Identity number: the 11-digit TC kimlik no for Turkish buyers, or
    /// another national id / passport number (5 to 20 letters and digits).
    pub identity_number: Option<String>,
    pub ip: Option<String>,
}

/// Whether `country` names Turkey (ISO code or name).
fn is_turkey(country: &str) -> bool {
    matches!(country.trim().to_ascii_uppercase().as_str(), "TR" | "TUR" | "TURKEY" | "TURKIYE" | "TÜRKIYE")
        || country.trim().eq_ignore_ascii_case("türkiye")
}

/// A usable identity number for a buyer in `country`, or `None`.
pub fn valid_identity_number(raw: Option<&str>, country: &str) -> Option<String> {
    let n: String = raw?.chars().filter(|c| !c.is_whitespace()).collect();
    let ok = if is_turkey(country) {
        n.len() == 11 && n.bytes().all(|b| b.is_ascii_digit()) && !n.starts_with('0') && n != "11111111111"
    } else {
        (5..=20).contains(&n.len()) && n.bytes().all(|b| b.is_ascii_alphanumeric())
    };
    ok.then_some(n)
}

/// The customer fields Iyzico needs that `buyer` lacks (empty = complete).
pub fn missing_buyer_fields(buyer: &IyzicoBuyer) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if buyer.name.trim().is_empty() {
        missing.push("name");
    }
    if !buyer.email.as_deref().is_some_and(|e| e.contains('@')) {
        missing.push("email");
    }
    if buyer.country.trim().is_empty() {
        missing.push("country");
    }
    if buyer.city.trim().is_empty() {
        missing.push("city");
    }
    if buyer.address.trim().is_empty() {
        missing.push("address_line1");
    }
    if valid_identity_number(buyer.identity_number.as_deref(), &buyer.country).is_none() {
        missing.push("identity_number");
    }
    missing
}

/// A resolved (authenticated) checkout form or payment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IyzicoPayment {
    /// Iyzico `paymentId` (numeric, as a string). Empty when not paid.
    pub payment_id: String,
    pub status: PayableStatus,
    /// Raw `paymentStatus` (`SUCCESS`, `FAILURE`, `INIT_THREEDS`, ...).
    pub provider_status: String,
    /// `basketId` (the Rago invoice id).
    pub basket_id: Option<String>,
    /// `paidPrice` in minor units.
    pub paid_minor: Option<i64>,
    pub currency: Option<String>,
}

/// `PaymentProviders::Iyzico` over an injectable transport `T`.
pub struct IyzicoProvider<T: HttpTransport> {
    api_key: String,
    secret_key: String,
    base: &'static str,
    callback_url: Option<String>,
    buyer: Option<IyzicoBuyer>,
    transport: T,
}

impl<T: HttpTransport> IyzicoProvider<T> {
    /// Sandbox is selected by a `sandbox-` API key prefix unless overridden.
    pub fn new(api_key: impl Into<String>, secret_key: impl Into<String>, transport: T) -> Self {
        let api_key = api_key.into();
        let base = if api_key.starts_with("sandbox-") { SANDBOX_BASE } else { LIVE_BASE };
        Self {
            api_key,
            secret_key: secret_key.into(),
            base,
            callback_url: None,
            buyer: None,
            transport,
        }
    }

    pub fn with_sandbox(mut self, sandbox: bool) -> Self {
        self.base = if sandbox { SANDBOX_BASE } else { LIVE_BASE };
        self
    }

    /// Rago's public `/webhooks/iyzico/:organization_id` URL.
    pub fn with_callback_url(mut self, url: impl Into<String>) -> Self {
        self.callback_url = Some(url.into());
        self
    }

    pub fn with_buyer(mut self, buyer: IyzicoBuyer) -> Self {
        self.buyer = Some(buyer);
        self
    }

    fn random_key() -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        format!("{nanos}{}", NONCE.fetch_add(1, Ordering::Relaxed))
    }

    /// Sign + POST to `uri_path`, returning the parsed JSON (any HTTP status;
    /// Iyzico reports failures in `status`/`errorMessage`).
    async fn signed_post(&self, uri_path: &str, body: &Value) -> Result<Value, PaymentError> {
        let raw = body.to_string();
        let rnd = Self::random_key();
        let authz = iyzws_v2_sign(&self.api_key, &self.secret_key, &rnd, uri_path, raw.as_bytes());
        let headers = vec![
            ("Authorization".to_string(), authz),
            ("x-iyzi-rnd".to_string(), rnd),
            ("Content-Type".to_string(), "application/json".to_string()),
        ];
        let resp = self
            .transport
            .post(&format!("{}{uri_path}", self.base), &headers, raw)
            .await?;
        serde_json::from_str(&resp.body).map_err(|e| PaymentError::InvalidResponse(e.to_string()))
    }

    /// Resolve a checkout-form `token` through the signed detail call.
    pub async fn retrieve_checkout(&self, token: &str) -> Result<IyzicoPayment, PaymentError> {
        let v = self
            .signed_post(
                "/payment/iyzipos/checkoutform/auth/ecom/detail",
                &json!({ "locale": "en", "token": token }),
            )
            .await?;
        payment_from_detail(&v)
    }

    /// The signed `/payment/detail` of a settled `paymentId`.
    pub async fn payment_detail(&self, payment_id: &str) -> Result<Value, PaymentError> {
        self.signed_post("/payment/detail", &json!({ "locale": "en", "paymentId": payment_id }))
            .await
    }
}

/// Interpret a checkout-form or payment detail response.
pub fn payment_from_detail(v: &Value) -> Result<IyzicoPayment, PaymentError> {
    if v["status"].as_str() != Some("success") {
        return Err(PaymentError::Provider {
            code: text(&v["errorCode"]),
            message: text(&v["errorMessage"]).unwrap_or_else(|| "iyzico request failed".into()),
        });
    }
    let provider_status = text(&v["paymentStatus"]).unwrap_or_default();
    let status = match provider_status.as_str() {
        "SUCCESS" => PayableStatus::Succeeded,
        "FAILURE" => PayableStatus::Failed,
        _ => PayableStatus::Pending,
    };
    Ok(IyzicoPayment {
        payment_id: text(&v["paymentId"]).unwrap_or_default(),
        status,
        provider_status,
        basket_id: text(&v["basketId"]),
        paid_minor: v.get("paidPrice").and_then(decimal_to_minor),
        currency: text(&v["currency"]),
    })
}

#[async_trait]
impl<T: HttpTransport> PaymentProvider for IyzicoProvider<T> {
    fn payment_type(&self) -> &'static str {
        "iyzico"
    }

    async fn create_payment(&self, req: &PaymentRequest) -> Result<PaymentOutcome, PaymentError> {
        let currency = req.currency.to_uppercase();
        if !SUPPORTED_CURRENCIES.contains(&currency.as_str()) {
            return Err(PaymentError::Provider {
                code: Some("currency_not_supported".into()),
                message: format!("Iyzico does not settle {currency}"),
            });
        }
        let callback_url = self.callback_url.clone().filter(|u| !u.is_empty()).ok_or_else(|| {
            PaymentError::Provider {
                code: Some("callback_url_missing".into()),
                message: "set RAGO_PUBLIC_URL so Iyzico can return the buyer to Rago".into(),
            }
        })?;
        let buyer = self.buyer.clone().unwrap_or_default();
        let missing = missing_buyer_fields(&buyer);
        if !missing.is_empty() {
            return Err(PaymentError::Provider {
                code: Some("customer_details_missing".into()),
                message: format!(
                    "Iyzico needs the customer's {}; add them to the customer record and retry",
                    missing.join(", ")
                ),
            });
        }
        let email = buyer.email.clone().unwrap_or_default();
        let identity = valid_identity_number(buyer.identity_number.as_deref(), &buyer.country).unwrap_or_default();
        let (first, last) = split_name(&buyer.name, &req.provider_customer_id);
        let price = minor_to_decimal(req.amount_cents);
        let full_name = format!("{first} {last}");
        let body = json!({
            "locale": "en",
            "conversationId": req.payment_id.to_string(),
            "price": price,
            "paidPrice": price,
            "currency": currency,
            "basketId": req.invoice_id.to_string(),
            "paymentGroup": "SUBSCRIPTION",
            "callbackUrl": callback_url,
            "enabledInstallments": [1],
            "buyer": {
                "id": if buyer.id.is_empty() { req.customer_id.to_string() } else { buyer.id.clone() },
                "name": first,
                "surname": last,
                "email": email,
                "identityNumber": identity,
                "registrationAddress": buyer.address,
                "city": buyer.city,
                "country": buyer.country,
                "zipCode": buyer.zip_code,
                "gsmNumber": buyer.phone,
                "ip": buyer.ip.clone().unwrap_or_else(|| "127.0.0.1".into()),
            },
            "billingAddress": {
                "contactName": full_name,
                "city": buyer.city,
                "country": buyer.country,
                "address": buyer.address,
                "zipCode": buyer.zip_code,
            },
            "basketItems": [{
                "id": req.reference,
                "name": format!("Invoice {}", req.reference),
                "category1": "Invoice",
                "itemType": "VIRTUAL",
                "price": price,
            }],
        });
        let v = self
            .signed_post("/payment/iyzipos/checkoutform/initialize/auth/ecom", &body)
            .await?;
        if v["status"].as_str() != Some("success") {
            return Err(PaymentError::Provider {
                code: text(&v["errorCode"]),
                message: text(&v["errorMessage"]).unwrap_or_else(|| "iyzico checkout refused".into()),
            });
        }
        let token = text(&v["token"])
            .ok_or_else(|| PaymentError::InvalidResponse("missing checkout token".into()))?;
        let page = text(&v["paymentPageUrl"])
            .filter(|u| u.starts_with("https://"))
            .ok_or_else(|| PaymentError::InvalidResponse("missing paymentPageUrl".into()))?;
        Ok(PaymentOutcome {
            provider_payment_id: token,
            provider_status: "INIT_CHECKOUT".into(),
            payable_status: PayableStatus::Pending,
            next_action: Some(super::redirect_next_action(&page)),
        })
    }

    async fn refund_payment(&self, req: &RefundRequest) -> Result<RefundOutcome, PaymentError> {
        if req.provider_payment_id.is_empty()
            || !req.provider_payment_id.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(PaymentError::Provider {
                code: Some("payment_not_settled".into()),
                message: "this Iyzico checkout has no settled paymentId to refund".into(),
            });
        }
        let detail = self.payment_detail(&req.provider_payment_id).await?;
        payment_from_detail(&detail)?;
        let currency = text(&detail["currency"]).unwrap_or_else(|| req.currency.to_uppercase());
        let item = detail["itemTransactions"]
            .as_array()
            .and_then(|items| {
                items.iter().find(|it| {
                    it.get("paidPrice").and_then(decimal_to_minor).is_some_and(|p| p >= req.amount_cents)
                        && !it["paymentTransactionId"].is_null()
                })
            })
            .ok_or_else(|| PaymentError::Provider {
                code: Some("refund_exceeds_item".into()),
                message: "no Iyzico item transaction covers this refund amount".into(),
            })?;
        let transaction_id = text(&item["paymentTransactionId"])
            .ok_or_else(|| PaymentError::InvalidResponse("missing paymentTransactionId".into()))?;
        let conversation = super::refund_idempotency_key(&req.refund_id);
        let v = self
            .signed_post(
                "/payment/refund",
                &json!({
                    "locale": "en",
                    "conversationId": conversation,
                    "paymentTransactionId": transaction_id,
                    "price": minor_to_decimal(req.amount_cents),
                    "currency": currency,
                    "ip": "127.0.0.1",
                }),
            )
            .await?;
        if v["status"].as_str() != Some("success") {
            return Err(PaymentError::Provider {
                code: text(&v["errorCode"]),
                message: text(&v["errorMessage"]).unwrap_or_else(|| "iyzico refund refused".into()),
            });
        }
        Ok(RefundOutcome {
            provider_refund_id: conversation,
            provider_status: "success".into(),
            state: RefundState::Succeeded,
        })
    }
}

/// `IYZWSv2 base64("apiKey:..&randomKey:..&signature:hex(hmac(secret, rnd+path+body))")`.
pub fn iyzws_v2_sign(api_key: &str, secret_key: &str, random_key: &str, uri_path: &str, body: &[u8]) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret_key.as_bytes()).expect("HMAC any key size");
    mac.update(random_key.as_bytes());
    mac.update(uri_path.as_bytes());
    mac.update(body);
    let sig: String = mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
    let authz = format!("apiKey:{api_key}&randomKey:{random_key}&signature:{sig}");
    format!("IYZWSv2 {}", base64::engine::general_purpose::STANDARD.encode(authz))
}

/// Minor units → Iyzico decimal string (`2599` → `"25.99"`).
pub fn minor_to_decimal(minor: i64) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let abs = minor.unsigned_abs();
    format!("{sign}{}.{:02}", abs / 100, abs % 100)
}

/// A decimal amount (string or number) → minor units, exactly (no floats).
pub fn decimal_to_minor(v: &Value) -> Option<i64> {
    let s = match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        _ => return None,
    };
    let (int_part, frac) = s.split_once('.').unwrap_or((&s, ""));
    if int_part.is_empty() || !int_part.bytes().all(|b| b.is_ascii_digit()) || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut cents: String = frac.chars().take(2).collect();
    while cents.len() < 2 {
        cents.push('0');
    }
    int_part.parse::<i64>().ok()?.checked_mul(100)?.checked_add(cents.parse::<i64>().ok()?)
}

fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn split_name(name: &str, fallback: &str) -> (String, String) {
    let name = name.trim();
    let name = if name.is_empty() { fallback.trim() } else { name };
    match name.rsplit_once(' ') {
        Some((first, last)) if !first.trim().is_empty() && !last.is_empty() => {
            (first.trim().to_string(), last.to_string())
        }
        _ => (name.to_string(), name.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::{HttpResponse, MockTransport};
    use uuid::Uuid;

    fn buyer() -> IyzicoBuyer {
        IyzicoBuyer {
            id: "cust_1".into(),
            name: "Ayse Yilmaz".into(),
            email: Some("ayse@example.com".into()),
            country: "Turkey".into(),
            city: "Istanbul".into(),
            address: "Levent Mah. 1".into(),
            zip_code: Some("34330".into()),
            phone: Some("+905350000000".into()),
            identity_number: Some("10000000146".into()),
            ip: None,
        }
    }

    #[tokio::test]
    async fn checkout_form_is_signed_and_returns_the_payment_page() {
        let t = MockTransport::always(
            200,
            r#"{"status":"success","token":"tok_1","paymentPageUrl":"https://sandbox-cpp.iyzipay.com?token=tok_1"}"#,
        );
        let p = IyzicoProvider::new("sandbox-key", "sandbox-secret", &t)
            .with_callback_url("https://rago.test/webhooks/iyzico/org")
            .with_buyer(buyer());
        let req = PaymentRequest::new(Uuid::nil(), Uuid::from_u128(5), Uuid::from_u128(6), "ext", 12_550, "try", "INV-9");
        let out = p.create_payment(&req).await.unwrap();
        assert_eq!(out.provider_payment_id, "tok_1");
        assert_eq!(out.payable_status, PayableStatus::Pending);
        let sent = t.last_request().unwrap();
        assert_eq!(sent.url, "https://sandbox-api.iyzipay.com/payment/iyzipos/checkoutform/initialize/auth/ecom");
        let authz = sent.headers.iter().find(|(k, _)| k == "Authorization").unwrap().1.clone();
        assert!(authz.starts_with("IYZWSv2 "));
        let body: Value = serde_json::from_str(&sent.body).unwrap();
        assert_eq!(body["price"], "125.50");
        assert_eq!(body["currency"], "TRY");
        assert_eq!(body["basketId"], Uuid::from_u128(5).to_string());
        assert_eq!(body["buyer"]["surname"], "Yilmaz");
        assert_eq!(body["buyer"]["identityNumber"], "10000000146");
        assert_eq!(body["buyer"]["city"], "Istanbul");
        assert_eq!(body["billingAddress"]["zipCode"], "34330");
        // The signature covers exactly the body that was sent.
        let rnd = sent.headers.iter().find(|(k, _)| k == "x-iyzi-rnd").unwrap().1.clone();
        assert_eq!(
            authz,
            iyzws_v2_sign(
                "sandbox-key",
                "sandbox-secret",
                &rnd,
                "/payment/iyzipos/checkoutform/initialize/auth/ecom",
                sent.body.as_bytes()
            )
        );
    }

    #[tokio::test]
    async fn missing_email_or_callback_is_a_clear_decline() {
        let t = MockTransport::always(500, "{}");
        let req = PaymentRequest::new(Uuid::nil(), Uuid::nil(), Uuid::nil(), "ext", 100, "TRY", "I");
        let no_cb = IyzicoProvider::new("k", "s", &t).with_buyer(buyer());
        assert!(matches!(no_cb.create_payment(&req).await, Err(PaymentError::Provider { code: Some(c), .. }) if c == "callback_url_missing"));
        let mut b = buyer();
        b.email = None;
        b.city = " ".into();
        b.identity_number = Some("11111111111".into());
        let incomplete = IyzicoProvider::new("k", "s", &t).with_callback_url("https://x").with_buyer(b);
        match incomplete.create_payment(&req).await {
            Err(PaymentError::Provider { code: Some(c), message }) => {
                assert_eq!(c, "customer_details_missing");
                assert!(message.contains("email, city, identity_number"), "{message}");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
        assert!(t.requests().is_empty());
    }

    #[test]
    fn identity_numbers_follow_the_buyer_country() {
        assert_eq!(valid_identity_number(Some("10000000146"), "TR").as_deref(), Some("10000000146"));
        assert!(valid_identity_number(Some("11111111111"), "Turkey").is_none());
        assert!(valid_identity_number(Some("1234"), "TR").is_none());
        assert_eq!(valid_identity_number(Some("X1234567"), "DE").as_deref(), Some("X1234567"));
        assert!(valid_identity_number(None, "DE").is_none());
    }

    #[tokio::test]
    async fn refund_uses_the_item_transaction_and_succeeds_synchronously() {
        let ok = |b: &str| Ok(HttpResponse { status: 200, body: b.into() });
        let t = MockTransport::scripted(
            vec![
                ok(r#"{"status":"success","paymentStatus":"SUCCESS","paymentId":"1234","currency":"TRY","paidPrice":"125.5","itemTransactions":[{"paymentTransactionId":"9876","paidPrice":"125.5"}]}"#),
                ok(r#"{"status":"success","paymentTransactionId":"9876","price":50.0}"#),
            ],
            HttpResponse { status: 500, body: "{}".into() },
        );
        let p = IyzicoProvider::new("k", "s", &t);
        let out = p
            .refund_payment(&RefundRequest {
                refund_id: Uuid::from_u128(4),
                provider_payment_id: "1234".into(),
                amount_cents: 5_000,
                currency: "TRY".into(),
                total_refunded_cents: 5_000,
                reason: None,
                reference: "CN".into(),
            })
            .await
            .unwrap();
        assert_eq!(out.state, RefundState::Succeeded);
        assert_eq!(out.provider_refund_id, format!("refund-{}", Uuid::from_u128(4)));
        let refund_body: Value = serde_json::from_str(&t.requests()[1].body).unwrap();
        assert_eq!(refund_body["paymentTransactionId"], "9876");
        assert_eq!(refund_body["price"], "50.00");
    }

    #[test]
    fn detail_maps_status_and_amount_exactly() {
        let p = payment_from_detail(&json!({"status":"success","paymentStatus":"SUCCESS","paymentId":555,"basketId":"inv","paidPrice":"10.1","currency":"TRY"})).unwrap();
        assert_eq!(p.payment_id, "555");
        assert_eq!(p.status, PayableStatus::Succeeded);
        assert_eq!(p.paid_minor, Some(1_010));
        assert!(payment_from_detail(&json!({"status":"failure","errorMessage":"bad token"})).is_err());
        assert_eq!(decimal_to_minor(&json!("0.5")), Some(50));
        assert_eq!(decimal_to_minor(&json!("abc")), None);
        assert_eq!(minor_to_decimal(7), "0.07");
    }
}
