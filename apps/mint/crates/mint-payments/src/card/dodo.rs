//! Dodo Payments adapter (merchant-of-record, billing-only) — the second MoR
//! next to Creem so a single vendor can never block deposits.
//!
//! **On-ramp** — a hosted checkout session (`POST /checkouts`) for the
//! configured pay-what-you-want credit product (`DODO_PRODUCT_ID`): one unit
//! whose `amount` is the deposit in cents, billed in USD, with the Mint account
//! threaded through `metadata.account_id`. The session id is only a checkout
//! handle; deposits are keyed on the `payment_id` the crediting webhook carries.
//!
//! **Re-confirmation** — [`Ramp::confirm_deposit`] re-reads `GET
//! /payments/{payment_id}` and binds the credit to the payment's
//! `metadata.account_id`, so a replayed or forged webhook cannot redirect it.
//!
//! **Webhooks** — Standard Webhooks (`webhook-id` / `webhook-timestamp` /
//! `webhook-signature`, base64 HMAC-SHA256 over `"{id}.{ts}.{body}"`), with a
//! five-minute timestamp tolerance. `payment.succeeded` ⇒ deposit;
//! `refund.succeeded` ⇒ refund reversal (cumulative, resolved from the payment's
//! refund list when partial); `dispute.opened` ⇒ dispute reversal.
//!
//! **Refund** — [`DodoRamp::refund_payment`] issues `POST /refunds` (full, or a
//! partial refund of the credit product line). Dodo has no arbitrary-payout
//! API, so `initiate_payout` is `ProviderUnavailable`.
//!
//! Amounts: Dodo may present the checkout in the buyer's local currency
//! (adaptive currency). The credited amount is always the USD settlement
//! (`settlement_amount` net of `settlement_tax`), falling back to
//! `total_amount` net of `tax` only when the payment itself is in USD. Anything
//! else fails closed.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use mint_core::Micros;
use mint_reserve::{Asset, Deposit, Payout, PayoutDest, Ramp, RampError};
use serde_json::Value;

use crate::card::{Checkout, CheckoutUrls};
use crate::config::{Mode, ProviderConfig};
use crate::event::{
    DepositEvent, ReversalEvent, ReversalReason, VerifiedEvent, WebhookError,
};
use crate::http::{map_status, Headers, HttpTransport};
use crate::money::cents_to_micros;
use crate::provider::ProviderId;
use crate::verify::standard_webhooks_verify;
use crate::PaymentError;

/// The only settlement currency Mint credits.
const DEPOSIT_CURRENCY: &str = "USD";

/// Standard Webhooks replay window (seconds either side of now).
pub const TIMESTAMP_TOLERANCE_SECS: i64 = 300;

/// The Dodo API base for a mode.
pub fn api_base(mode: Mode) -> &'static str {
    match mode {
        Mode::Sandbox => "https://test.dodopayments.com",
        Mode::Live => "https://live.dodopayments.com",
    }
}

/// A live Dodo Payments [`Ramp`] over an injected [`HttpTransport`].
pub struct DodoRamp {
    transport: Arc<dyn HttpTransport>,
    cfg: ProviderConfig,
}

/// The outcome of a Dodo refund request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DodoRefund {
    /// Dodo refund id.
    pub refund_id: String,
    /// `succeeded` | `pending` | `review` | `failed`.
    pub status: String,
}

impl DodoRamp {
    /// Build the adapter from its transport + resolved config.
    pub fn new(transport: Arc<dyn HttpTransport>, cfg: ProviderConfig) -> Self {
        DodoRamp { transport, cfg }
    }

    fn base(&self) -> &'static str {
        api_base(self.cfg.mode)
    }

    fn auth_headers(&self) -> Vec<(String, String)> {
        let key = self.cfg.api_key.clone().unwrap_or_default();
        vec![("authorization".into(), format!("Bearer {key}"))]
    }

    /// Create a Dodo checkout session crediting `account_id` for
    /// `amount_micros` (a whole number of cents).
    pub async fn create_checkout(
        &self,
        account_id: &str,
        amount_micros: i64,
        urls: &CheckoutUrls,
    ) -> Result<Checkout, PaymentError> {
        let product_id = self.cfg.extra("product_id").ok_or_else(|| {
            PaymentError::Misconfigured(
                "DODO_PRODUCT_ID must name a pay-what-you-want AIT credit product".into(),
            )
        })?;
        if !crate::card::is_whole_cents(amount_micros) {
            return Err(PaymentError::InvalidRequest(
                "Dodo deposits must be a positive whole number of cents".into(),
            ));
        }
        let body = serde_json::json!({
            "product_cart": [{
                "product_id": product_id,
                "quantity": 1,
                "amount": amount_micros / 10_000,
            }],
            "billing_currency": DEPOSIT_CURRENCY,
            "return_url": urls.success_url,
            "cancel_url": urls.cancel_url,
            "metadata": { "account_id": account_id },
        });
        let mut headers = self.auth_headers();
        headers.push(("content-type".into(), "application/json".into()));
        let url = format!("{}/checkouts", self.base());
        let raw = serde_json::to_vec(&body).expect("json");
        let resp = map_status(self.transport.post_json(&url, &headers, &raw).await)?;
        let v: Value = serde_json::from_slice(&resp.body)
            .map_err(|_| PaymentError::Transport("Dodo returned a non-JSON checkout".into()))?;
        let checkout_url = v["checkout_url"].as_str().unwrap_or_default().to_string();
        let checkout_id = v["session_id"].as_str().unwrap_or_default().to_string();
        if checkout_url.is_empty() || checkout_id.is_empty() {
            return Err(PaymentError::Transport(
                "Dodo checkout response is missing session_id/checkout_url".into(),
            ));
        }
        Ok(Checkout {
            checkout_url,
            checkout_id,
        })
    }

    /// `GET /payments/{payment_id}`.
    async fn payment(&self, payment_id: &str) -> Result<Value, RampError> {
        let url = format!(
            "{}/payments/{}",
            self.base(),
            crate::http::encode_component(payment_id)
        );
        let resp = map_status(self.transport.get(&url, &self.auth_headers()).await)?;
        serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)
    }

    /// Refund `amount_cents` of a Dodo payment (`None` = the whole payment).
    /// A partial refund targets the credit product line, which is the only
    /// line Mint's checkouts create. `idempotency_ref` rides in `metadata` so
    /// the refund is traceable to the Mint operation that issued it.
    pub async fn refund_payment(
        &self,
        payment_id: &str,
        amount_cents: Option<i64>,
        reason: &str,
        idempotency_ref: &str,
    ) -> Result<DodoRefund, PaymentError> {
        let mut body = serde_json::json!({
            "payment_id": payment_id,
            "reason": reason,
            "metadata": { "mint_ref": idempotency_ref },
        });
        if let Some(cents) = amount_cents {
            if cents <= 0 {
                return Err(PaymentError::InvalidRequest(
                    "refund amount must be positive".into(),
                ));
            }
            let product_id = self.cfg.extra("product_id").ok_or_else(|| {
                PaymentError::Misconfigured(
                    "DODO_PRODUCT_ID is required for partial refunds".into(),
                )
            })?;
            body["items"] = serde_json::json!([{ "item_id": product_id, "amount": cents }]);
        }
        let mut headers = self.auth_headers();
        headers.push(("content-type".into(), "application/json".into()));
        headers.push(("idempotency-key".into(), idempotency_ref.to_string()));
        let url = format!("{}/refunds", self.base());
        let raw = serde_json::to_vec(&body).expect("json");
        let resp = map_status(self.transport.post_json(&url, &headers, &raw).await)?;
        let v: Value = serde_json::from_slice(&resp.body)
            .map_err(|_| PaymentError::Transport("Dodo returned a non-JSON refund".into()))?;
        let refund_id = v["refund_id"].as_str().unwrap_or_default().to_string();
        let status = v["status"].as_str().unwrap_or_default().to_string();
        if refund_id.is_empty() || status.is_empty() {
            return Err(PaymentError::Transport(
                "Dodo refund response is missing refund_id/status".into(),
            ));
        }
        Ok(DodoRefund { refund_id, status })
    }

    /// Verify + classify one delivery. Deposits and disputes classify from the
    /// signed payload alone; a PARTIAL refund re-reads the payment so the
    /// reversal amount is cumulative across every succeeded refund.
    pub async fn verify_event<H: Headers>(
        &self,
        headers: &H,
        raw: &[u8],
    ) -> Result<VerifiedEvent, WebhookError> {
        verify_signature(&self.cfg, headers, raw, now_secs())?;
        let v: Value =
            serde_json::from_slice(raw).map_err(|e| WebhookError::Malformed(e.to_string()))?;
        let event_type = v["type"].as_str().unwrap_or("").to_string();
        let data = &v["data"];
        match event_type.as_str() {
            "payment.succeeded" => deposit_event(data).map(VerifiedEvent::Deposit),
            "refund.succeeded" => {
                let payment_id = str_field(data, "payment_id")
                    .ok_or_else(|| WebhookError::Malformed("missing data.payment_id".into()))?;
                let amount_micros = if data["is_partial"].as_bool() == Some(true) {
                    let payment = self.payment(&payment_id).await.map_err(|_| {
                        WebhookError::Malformed("dodo payment lookup unavailable".into())
                    })?;
                    let cents = succeeded_refund_cents(&payment)
                        .or_else(|| int_field(data, "amount"))
                        .ok_or_else(|| WebhookError::Malformed("missing refund amount".into()))?;
                    Some(
                        cents_to_micros(cents.abs())
                            .ok_or_else(|| WebhookError::Malformed("amount overflow".into()))?,
                    )
                } else {
                    None
                };
                Ok(VerifiedEvent::Reversal(ReversalEvent {
                    provider: ProviderId::Dodo,
                    deposit_ref: payment_id,
                    alt_refs: Vec::new(),
                    amount_micros,
                    reason: ReversalReason::Refund,
                }))
            }
            "dispute.opened" => dispute_event(data).map(VerifiedEvent::Reversal),
            other => Err(WebhookError::Ignored(other.to_string())),
        }
    }
}

#[async_trait]
impl Ramp for DodoRamp {
    async fn confirm_deposit(&self, deposit_ref: &str) -> Result<Deposit, RampError> {
        let v = self.payment(deposit_ref).await?;
        if v["status"].as_str() != Some("succeeded") {
            return Err(RampError::NotConfirmed);
        }
        if str_field(&v, "payment_id").as_deref() != Some(deposit_ref) {
            return Err(RampError::NotConfirmed);
        }
        let cents = usd_net_cents(&v).ok_or(RampError::NotConfirmed)?;
        let micros = cents_to_micros(cents).ok_or(RampError::ProviderUnavailable)?;
        // Bind the credit to the account Mint put on the checkout; a payment
        // Mint did not create (no account metadata) is never credited.
        let bound_account = Some(
            v["metadata"]["account_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or(RampError::NotConfirmed)?
                .to_string(),
        );
        Ok(Deposit {
            deposit_ref: deposit_ref.to_string(),
            amount_micros: Micros::new(micros),
            asset: Asset::Usd,
            confirmed: true,
            bound_account,
        })
    }

    async fn initiate_payout(
        &self,
        _payout_ref: &str,
        _dest: &PayoutDest,
        _amount: Micros,
    ) -> Result<Payout, RampError> {
        // Merchant of record: there is no arbitrary-payout API.
        Err(RampError::ProviderUnavailable)
    }

    async fn check_payout(
        &self,
        _payout_ref: &str,
        _dest: &PayoutDest,
        _amount: Micros,
    ) -> Result<(), RampError> {
        // Billing-only provider: refuse BEFORE the redeem burns anything.
        Err(RampError::Unsupported)
    }
}

/// Verify a Dodo delivery over RAW bytes and return the crediting deposit
/// (`payment.succeeded`). Every other event is `Ignored` here; use
/// [`DodoRamp::verify_event`] for reversals.
pub fn verify_webhook<H: Headers>(
    cfg: &ProviderConfig,
    headers: &H,
    raw: &[u8],
) -> Result<DepositEvent, WebhookError> {
    verify_webhook_at(cfg, headers, raw, now_secs())
}

/// [`verify_webhook`] against an explicit clock (tests).
pub fn verify_webhook_at<H: Headers>(
    cfg: &ProviderConfig,
    headers: &H,
    raw: &[u8],
    now: i64,
) -> Result<DepositEvent, WebhookError> {
    verify_signature(cfg, headers, raw, now)?;
    let v: Value =
        serde_json::from_slice(raw).map_err(|e| WebhookError::Malformed(e.to_string()))?;
    let event_type = v["type"].as_str().unwrap_or("");
    if event_type != "payment.succeeded" {
        return Err(WebhookError::Ignored(event_type.to_string()));
    }
    deposit_event(&v["data"])
}

/// Verify a Dodo delivery and, when it is a FULL refund or a dispute, return
/// the reversal. Partial refunds need the payment's refund history, which only
/// the transport-backed [`DodoRamp::verify_event`] can read; here they fail
/// closed as `Malformed` so the provider retries through the full path.
pub fn verify_reversal_webhook<H: Headers>(
    cfg: &ProviderConfig,
    headers: &H,
    raw: &[u8],
) -> Result<ReversalEvent, WebhookError> {
    verify_signature(cfg, headers, raw, now_secs())?;
    let v: Value =
        serde_json::from_slice(raw).map_err(|e| WebhookError::Malformed(e.to_string()))?;
    let event_type = v["type"].as_str().unwrap_or("");
    let data = &v["data"];
    match event_type {
        "refund.succeeded" => {
            if data["is_partial"].as_bool() == Some(true) {
                return Err(WebhookError::Malformed(
                    "partial dodo refund needs the payment refund history".into(),
                ));
            }
            let payment_id = str_field(data, "payment_id")
                .ok_or_else(|| WebhookError::Malformed("missing data.payment_id".into()))?;
            Ok(ReversalEvent {
                provider: ProviderId::Dodo,
                deposit_ref: payment_id,
                alt_refs: Vec::new(),
                amount_micros: None,
                reason: ReversalReason::Refund,
            })
        }
        "dispute.opened" => dispute_event(data),
        other => Err(WebhookError::Ignored(other.to_string())),
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

/// Standard Webhooks signature + replay-window check over the RAW body.
fn verify_signature<H: Headers>(
    cfg: &ProviderConfig,
    headers: &H,
    raw: &[u8],
    now: i64,
) -> Result<(), WebhookError> {
    let secret = cfg
        .webhook_secret
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or(WebhookError::NotConfigured)?;
    let id = headers
        .get_header("webhook-id")
        .ok_or(WebhookError::BadSignature)?;
    let ts = headers
        .get_header("webhook-timestamp")
        .ok_or(WebhookError::BadSignature)?;
    let sig = headers
        .get_header("webhook-signature")
        .ok_or(WebhookError::BadSignature)?;
    let ts_num: i64 = ts.trim().parse().map_err(|_| WebhookError::BadSignature)?;
    if (now - ts_num).abs() > TIMESTAMP_TOLERANCE_SECS {
        return Err(WebhookError::BadSignature);
    }
    standard_webhooks_verify(secret, id, ts, raw, sig)
}

/// Build the deposit from a signed `payment.succeeded` payload.
fn deposit_event(data: &Value) -> Result<DepositEvent, WebhookError> {
    if data["status"].as_str().is_some_and(|s| s != "succeeded") {
        return Err(WebhookError::Ignored("payment not succeeded".into()));
    }
    let deposit_ref = str_field(data, "payment_id")
        .ok_or_else(|| WebhookError::Malformed("missing data.payment_id".into()))?;
    let cents = usd_net_cents(data)
        .ok_or_else(|| WebhookError::Malformed("no USD settlement amount".into()))?;
    let amount_micros =
        cents_to_micros(cents).ok_or_else(|| WebhookError::Malformed("amount overflow".into()))?;
    let account_id = data["metadata"]["account_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| WebhookError::Malformed("missing metadata.account_id".into()))?
        .to_string();
    Ok(DepositEvent {
        provider: ProviderId::Dodo,
        deposit_ref,
        amount_micros,
        asset: Asset::Usd,
        account_id,
    })
}

/// A signed `dispute.opened` payload → dispute reversal for its amount.
fn dispute_event(data: &Value) -> Result<ReversalEvent, WebhookError> {
    let payment_id = str_field(data, "payment_id")
        .ok_or_else(|| WebhookError::Malformed("missing data.payment_id".into()))?;
    if let Some(currency) = data["currency"].as_str() {
        if !currency.eq_ignore_ascii_case(DEPOSIT_CURRENCY) {
            // A non-USD dispute amount cannot be clawed back 1:1; reverse the
            // whole deposit instead of guessing a conversion.
            return Ok(ReversalEvent {
                provider: ProviderId::Dodo,
                deposit_ref: payment_id,
                alt_refs: Vec::new(),
                amount_micros: None,
                reason: ReversalReason::Dispute,
            });
        }
    }
    let amount_micros = match int_field(data, "amount") {
        Some(c) if c != 0 => Some(
            cents_to_micros(c.abs())
                .ok_or_else(|| WebhookError::Malformed("amount overflow".into()))?,
        ),
        _ => None,
    };
    Ok(ReversalEvent {
        provider: ProviderId::Dodo,
        deposit_ref: payment_id,
        alt_refs: Vec::new(),
        amount_micros,
        reason: ReversalReason::Dispute,
    })
}

/// The USD amount (cents, net of tax) a payment settled for, or `None` when
/// the payment did not settle in USD.
pub fn usd_net_cents(payment: &Value) -> Option<i64> {
    let settled_in_usd = payment["settlement_currency"]
        .as_str()
        .is_some_and(|c| c.eq_ignore_ascii_case(DEPOSIT_CURRENCY));
    let gross_and_tax = if settled_in_usd {
        int_field(payment, "settlement_amount").map(|g| (g, int_field(payment, "settlement_tax")))
    } else if payment["currency"]
        .as_str()
        .is_some_and(|c| c.eq_ignore_ascii_case(DEPOSIT_CURRENCY))
    {
        int_field(payment, "total_amount").map(|g| (g, int_field(payment, "tax")))
    } else {
        None
    };
    let (gross, tax) = gross_and_tax?;
    let net = gross.checked_sub(tax.unwrap_or(0).max(0))?;
    (net > 0).then_some(net)
}

/// Sum of the payment's `succeeded` refunds (cents), if it lists any.
fn succeeded_refund_cents(payment: &Value) -> Option<i64> {
    let refunds = payment["refunds"].as_array()?;
    let total: i64 = refunds
        .iter()
        .filter(|r| r["status"].as_str() == Some("succeeded"))
        .filter_map(|r| int_field(r, "amount"))
        .map(i64::abs)
        .sum();
    (total > 0).then_some(total)
}

fn str_field(v: &Value, key: &str) -> Option<String> {
    v[key].as_str().filter(|s| !s.is_empty()).map(str::to_string)
}

/// Dodo sends minor-unit amounts as integers (disputes as numeric strings).
fn int_field(v: &Value, key: &str) -> Option<i64> {
    match &v[key] {
        Value::Number(n) => n.as_i64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::hmac_sha256_raw;
    use base64::engine::general_purpose::STANDARD as B64;
    use base64::Engine;
    use std::collections::HashMap;
    use std::sync::Mutex;

    use crate::http::{HttpResponse, TransportError};

    const RAW_KEY: &[u8] = b"dodo-test-signing-key";

    fn cfg() -> ProviderConfig {
        let mut extra = HashMap::new();
        extra.insert("product_id".to_string(), "pdt_credit".to_string());
        ProviderConfig {
            provider: ProviderId::Dodo,
            api_key: Some("dodo_test_key".into()),
            webhook_secret: Some(format!("whsec_{}", B64.encode(RAW_KEY))),
            mode: Mode::Sandbox,
            extra,
        }
    }

    fn signed(body: &[u8], ts: i64) -> HashMap<String, String> {
        let id = "msg_1";
        let ts = ts.to_string();
        let mut signed = Vec::new();
        signed.extend_from_slice(id.as_bytes());
        signed.push(b'.');
        signed.extend_from_slice(ts.as_bytes());
        signed.push(b'.');
        signed.extend_from_slice(body);
        let tag = B64.encode(hmac_sha256_raw(RAW_KEY, &signed));
        HashMap::from([
            ("webhook-id".to_string(), id.to_string()),
            ("webhook-timestamp".to_string(), ts),
            ("webhook-signature".to_string(), format!("v1,{tag}")),
        ])
    }

    /// A recorded request: URL, headers, body.
    type Seen = (String, Vec<(String, String)>, Vec<u8>);

    /// Records every request and answers by URL substring.
    #[derive(Default)]
    struct Recorder {
        routes: Vec<(&'static str, u16, Value)>,
        seen: Mutex<Vec<Seen>>,
    }

    impl Recorder {
        fn route(mut self, pat: &'static str, status: u16, body: Value) -> Self {
            self.routes.push((pat, status, body));
            self
        }
        fn answer(&self, url: &str) -> Result<HttpResponse, TransportError> {
            for (pat, status, body) in &self.routes {
                if url.contains(pat) {
                    return Ok(HttpResponse {
                        status: *status,
                        body: serde_json::to_vec(body).unwrap(),
                    });
                }
            }
            Ok(HttpResponse {
                status: 404,
                body: b"{}".to_vec(),
            })
        }
    }

    #[async_trait]
    impl HttpTransport for Arc<Recorder> {
        async fn get(
            &self,
            url: &str,
            headers: &[(String, String)],
        ) -> Result<HttpResponse, TransportError> {
            self.seen
                .lock()
                .unwrap()
                .push((url.to_string(), headers.to_vec(), Vec::new()));
            self.answer(url)
        }
        async fn post_json(
            &self,
            url: &str,
            headers: &[(String, String)],
            body: &[u8],
        ) -> Result<HttpResponse, TransportError> {
            self.seen
                .lock()
                .unwrap()
                .push((url.to_string(), headers.to_vec(), body.to_vec()));
            self.answer(url)
        }
    }

    fn urls() -> CheckoutUrls {
        CheckoutUrls {
            success_url: "https://app.test/ok".into(),
            cancel_url: "https://app.test/cancel".into(),
            capture_return_url: None,
            public_base_url: None,
        }
    }

    #[tokio::test]
    async fn checkout_posts_pay_what_you_want_line_with_account_metadata() {
        let rec = Arc::new(Recorder::default().route(
            "/checkouts",
            200,
            serde_json::json!({"session_id":"cks_1","checkout_url":"https://test.checkout.dodopayments.com/cks_1"}),
        ));
        let ramp = DodoRamp::new(Arc::new(rec.clone()), cfg());
        let checkout = ramp
            .create_checkout("acct_9", 12_340_000, &urls())
            .await
            .unwrap();
        assert_eq!(checkout.checkout_id, "cks_1");
        let seen = rec.seen.lock().unwrap();
        let (url, headers, body) = &seen[0];
        assert_eq!(url, "https://test.dodopayments.com/checkouts");
        assert!(headers
            .iter()
            .any(|(k, v)| k == "authorization" && v == "Bearer dodo_test_key"));
        let body: Value = serde_json::from_slice(body).unwrap();
        assert_eq!(body["product_cart"][0]["product_id"], "pdt_credit");
        assert_eq!(body["product_cart"][0]["amount"], 1234);
        assert_eq!(body["billing_currency"], "USD");
        assert_eq!(body["metadata"]["account_id"], "acct_9");
    }

    #[tokio::test]
    async fn checkout_rejects_sub_cent_amounts_and_missing_product() {
        let ramp = DodoRamp::new(Arc::new(Arc::new(Recorder::default())), cfg());
        assert!(matches!(
            ramp.create_checkout("a", 12_345, &urls()).await,
            Err(PaymentError::InvalidRequest(_))
        ));
        let mut c = cfg();
        c.extra.clear();
        let ramp = DodoRamp::new(Arc::new(Arc::new(Recorder::default())), c);
        assert!(matches!(
            ramp.create_checkout("a", 10_000, &urls()).await,
            Err(PaymentError::Misconfigured(_))
        ));
    }

    #[test]
    fn payment_succeeded_yields_usd_net_deposit() {
        let body = br#"{"business_id":"bus_1","type":"payment.succeeded","timestamp":"2026-09-24T00:00:00Z","data":{"payload_type":"Payment","payment_id":"pay_1","status":"succeeded","currency":"EUR","total_amount":1100,"tax":100,"settlement_amount":1250,"settlement_tax":50,"settlement_currency":"USD","metadata":{"account_id":"acct_7"}}}"#;
        let now = 1_800_000_000;
        let ev = verify_webhook_at(&cfg(), &signed(body, now), body, now).unwrap();
        assert_eq!(ev.provider, ProviderId::Dodo);
        assert_eq!(ev.deposit_ref, "pay_1");
        assert_eq!(ev.amount_micros, 12_000_000);
        assert_eq!(ev.account_id, "acct_7");
    }

    #[test]
    fn stale_or_tampered_deliveries_are_rejected() {
        let body = br#"{"type":"payment.succeeded","data":{"payment_id":"pay_1","currency":"USD","total_amount":500,"metadata":{"account_id":"a"}}}"#;
        let now = 1_800_000_000;
        // Replayed outside the tolerance window.
        let old = signed(body, now - TIMESTAMP_TOLERANCE_SECS - 1);
        assert_eq!(
            verify_webhook_at(&cfg(), &old, body, now),
            Err(WebhookError::BadSignature)
        );
        // Body changed after signing.
        let hdrs = signed(body, now);
        let tampered = br#"{"type":"payment.succeeded","data":{"payment_id":"pay_1","currency":"USD","total_amount":900,"metadata":{"account_id":"a"}}}"#;
        assert_eq!(
            verify_webhook_at(&cfg(), &hdrs, tampered, now),
            Err(WebhookError::BadSignature)
        );
        // No secret configured.
        let mut c = cfg();
        c.webhook_secret = None;
        assert_eq!(
            verify_webhook_at(&c, &hdrs, body, now),
            Err(WebhookError::NotConfigured)
        );
    }

    #[test]
    fn non_usd_payment_without_usd_settlement_fails_closed() {
        let body = br#"{"type":"payment.succeeded","data":{"payment_id":"pay_2","currency":"INR","total_amount":90000,"metadata":{"account_id":"a"}}}"#;
        let now = 1_800_000_000;
        assert!(matches!(
            verify_webhook_at(&cfg(), &signed(body, now), body, now),
            Err(WebhookError::Malformed(_))
        ));
    }

    #[tokio::test]
    async fn partial_refund_reversal_is_cumulative_from_payment_history() {
        let rec = Arc::new(Recorder::default().route(
            "/payments/pay_3",
            200,
            serde_json::json!({"payment_id":"pay_3","refunds":[
                {"refund_id":"r1","status":"succeeded","amount":300},
                {"refund_id":"r2","status":"succeeded","amount":200},
                {"refund_id":"r3","status":"failed","amount":999}
            ]}),
        ));
        let ramp = DodoRamp::new(Arc::new(rec), cfg());
        let body = br#"{"type":"refund.succeeded","data":{"payload_type":"Refund","refund_id":"r2","payment_id":"pay_3","amount":200,"currency":"USD","is_partial":true,"status":"succeeded"}}"#;
        let ev = ramp.verify_event(&signed(body, now_secs()), body).await.unwrap();
        match ev {
            VerifiedEvent::Reversal(r) => {
                assert_eq!(r.deposit_ref, "pay_3");
                assert_eq!(r.amount_micros, Some(5_000_000));
                assert_eq!(r.reason, ReversalReason::Refund);
            }
            other => panic!("expected reversal, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn full_refund_and_dispute_reverse() {
        let ramp = DodoRamp::new(Arc::new(Arc::new(Recorder::default())), cfg());
        let body = br#"{"type":"refund.succeeded","data":{"refund_id":"r9","payment_id":"pay_4","amount":500,"is_partial":false}}"#;
        match ramp.verify_event(&signed(body, now_secs()), body).await.unwrap() {
            VerifiedEvent::Reversal(r) => assert_eq!(r.amount_micros, None),
            other => panic!("{other:?}"),
        }
        let body = br#"{"type":"dispute.opened","data":{"dispute_id":"dsp_1","payment_id":"pay_4","amount":"450","currency":"USD"}}"#;
        match ramp.verify_event(&signed(body, now_secs()), body).await.unwrap() {
            VerifiedEvent::Reversal(r) => {
                assert_eq!(r.reason, ReversalReason::Dispute);
                assert_eq!(r.amount_micros, Some(4_500_000));
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn confirm_deposit_binds_the_metadata_account() {
        let rec = Arc::new(Recorder::default().route(
            "/payments/pay_5",
            200,
            serde_json::json!({"payment_id":"pay_5","status":"succeeded","currency":"USD","total_amount":700,"tax":0,"metadata":{"account_id":"acct_5"}}),
        ).route(
            "/payments/pay_6",
            200,
            serde_json::json!({"payment_id":"pay_6","status":"processing","currency":"USD","total_amount":700}),
        ));
        let ramp = DodoRamp::new(Arc::new(rec), cfg());
        let dep = ramp.confirm_deposit("pay_5").await.unwrap();
        assert_eq!(dep.amount_micros, Micros::new(7_000_000));
        assert_eq!(dep.bound_account.as_deref(), Some("acct_5"));
        assert!(matches!(
            ramp.confirm_deposit("pay_6").await,
            Err(RampError::NotConfirmed)
        ));
        assert!(matches!(
            ramp.confirm_deposit("pay_missing").await,
            Err(RampError::DepositNotFound)
        ));
    }

    #[tokio::test]
    async fn refund_sends_partial_line_and_idempotency_ref() {
        let rec = Arc::new(Recorder::default().route(
            "/refunds",
            200,
            serde_json::json!({"refund_id":"ref_1","status":"pending","amount":250}),
        ));
        let ramp = DodoRamp::new(Arc::new(rec.clone()), cfg());
        let out = ramp
            .refund_payment("pay_7", Some(250), "requested_by_customer", "mint-refund-1")
            .await
            .unwrap();
        assert_eq!(out, DodoRefund { refund_id: "ref_1".into(), status: "pending".into() });
        let seen = rec.seen.lock().unwrap();
        let body: Value = serde_json::from_slice(&seen[0].2).unwrap();
        assert_eq!(body["payment_id"], "pay_7");
        assert_eq!(body["items"][0]["item_id"], "pdt_credit");
        assert_eq!(body["items"][0]["amount"], 250);
        assert_eq!(body["metadata"]["mint_ref"], "mint-refund-1");
    }

    #[tokio::test]
    async fn dodo_is_billing_only() {
        let ramp = DodoRamp::new(Arc::new(Arc::new(Recorder::default())), cfg());
        let dest = PayoutDest::Stripe { account: "acct_x".into() };
        assert!(matches!(
            ramp.check_payout("dodo:1", &dest, Micros::new(10_000)).await,
            Err(RampError::Unsupported)
        ));
    }
}
