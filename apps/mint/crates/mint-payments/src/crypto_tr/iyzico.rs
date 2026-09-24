//! Iyzico adapter (Turkish card acquirer).
//!
//! **On-ramp** — a hosted Checkout Form. [`IyzicoRamp::initialize_checkout`]
//! signs a `/payment/iyzipos/checkoutform/initialize/auth/ecom` request whose
//! `callbackUrl` is Mint's own `{MINT_PUBLIC_URL}/v1/deposits/iyzico/webhook`
//! and whose `basketId` is the Mint account. When the buyer finishes, Iyzico
//! returns the BROWSER to that URL with a form POST carrying only `token`; the
//! token is resolved through the IYZWSv2-signed
//! `/payment/iyzipos/checkoutform/auth/ecom/detail` call into the real
//! `paymentId`, `paidPrice` and `basketId`. Deposits are keyed on the
//! `paymentId` (the id refunds, merchant notifications and settlement reports
//! all carry), and [`Ramp::confirm_deposit`] re-verifies it via `/payment/detail`
//! and binds the credit to the payment's `basketId`.
//!
//! **Merchant notifications** — server-to-server JSON webhooks signed with
//! `X-IYZ-SIGNATURE-V3` (hex HMAC-SHA256 under the merchant secret key). A
//! successful payment notification mints (same `paymentId` key, so it and the
//! browser callback credit exactly once); refund / cancel / chargeback
//! notifications become reversals of that `paymentId`.
//!
//! **Off-ramp** — a refund of an original card payment. The payout ref names
//! the ORIGINAL `paymentId` (`iyzico:<paymentId>[:<suffix>]`); the item-level
//! `paymentTransactionId` and currency are looked up from `/payment/detail`.
//!
//! Mint credits USD: checkouts are created in USD and a payment settled in any
//! other currency is refused rather than minted 1:1.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use mint_core::Micros;
use mint_reserve::{Asset, Deposit, Payout, PayoutDest, PayoutStatus, Ramp, RampError};
use serde_json::Value;

use crate::card::Checkout;
use crate::config::{Mode, ProviderConfig};
use crate::crypto_tr::{micros_to_dollars, parse_form};
use crate::event::{DepositEvent, ReversalEvent, ReversalReason, VerifiedEvent, WebhookError};
use crate::http::{map_status, Headers, HttpTransport};
use crate::money::{decimal_to_micros, json_dollars_to_micros};
use crate::provider::ProviderId;
use crate::verify::{hmac_sha256_hex_verify, iyzws_v2_sign};

/// The only settlement currency Mint credits (1 USD = 1_000_000 µAIT).
const DEPOSIT_CURRENCY: &str = "USD";

/// Per-process counter mixed into the IYZWSv2 random key.
static NONCE: AtomicU64 = AtomicU64::new(0);

fn api_base(mode: Mode) -> &'static str {
    match mode {
        Mode::Sandbox => "https://sandbox-api.iyzipay.com",
        Mode::Live => "https://api.iyzipay.com",
    }
}

/// A fresh IYZWSv2 random key (time + counter; unique per request).
fn random_key() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{nanos}{}", NONCE.fetch_add(1, Ordering::Relaxed))
}

/// Read a money field Iyzico returns as either a decimal string or a number.
fn money(v: &Value) -> Option<i64> {
    json_dollars_to_micros(v).or_else(|| v.as_str().and_then(decimal_to_micros))
}

/// A settled payment as reported by an AUTHENTICATED Iyzico query.
struct SettledPayment {
    payment_id: String,
    amount_micros: i64,
    account_id: Option<String>,
}

/// Validate a `/payment/detail` or checkout-form detail response and extract the
/// settled payment. Fails closed on a non-success status, a non-USD currency
/// and a missing/zero `paidPrice`.
fn settled_payment(v: &Value) -> Result<SettledPayment, RampError> {
    if v["status"].as_str() != Some("success") || v["paymentStatus"].as_str() != Some("SUCCESS")
    {
        return Err(RampError::NotConfirmed);
    }
    if let Some(currency) = v["currency"].as_str() {
        if !currency.eq_ignore_ascii_case(DEPOSIT_CURRENCY) {
            tracing::warn!(currency, "iyzico payment settled in a non-USD currency; not minted");
            return Err(RampError::NotConfirmed);
        }
    }
    let payment_id = match &v["paymentId"] {
        Value::String(s) if !s.is_empty() => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => return Err(RampError::NotConfirmed),
    };
    let amount_micros = money(&v["paidPrice"])
        .filter(|&m| m > 0)
        .ok_or(RampError::NotConfirmed)?;
    // The account Mint put in `basketId` at checkout time. (`conversationId` in
    // a detail response echoes the DETAIL request, so it is not used.)
    let account_id = v["basketId"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    Ok(SettledPayment {
        payment_id,
        amount_micros,
        account_id,
    })
}

/// A live Iyzico [`Ramp`] over an injected [`HttpTransport`].
pub struct IyzicoRamp {
    transport: Arc<dyn HttpTransport>,
    cfg: ProviderConfig,
}

impl IyzicoRamp {
    /// Build the adapter from its transport + resolved config.
    pub fn new(transport: Arc<dyn HttpTransport>, cfg: ProviderConfig) -> Self {
        IyzicoRamp { transport, cfg }
    }

    fn base(&self) -> &'static str {
        api_base(self.cfg.mode)
    }

    fn secret_key(&self) -> Option<&str> {
        self.cfg.extra("secret_key").filter(|k| !k.is_empty())
    }

    /// Sign + POST an IYZWSv2-authenticated request to `uri_path`.
    async fn signed_post(&self, uri_path: &str, body: &[u8]) -> Result<Value, RampError> {
        let api_key = self.cfg.api_key.clone().unwrap_or_default();
        let secret_key = self
            .secret_key()
            .ok_or(RampError::ProviderUnavailable)?
            .to_string();
        let rnd = random_key();
        let authz = iyzws_v2_sign(&api_key, &secret_key, &rnd, uri_path, body);
        let headers = vec![
            ("authorization".to_string(), authz),
            ("x-iyzi-rnd".to_string(), rnd),
            ("content-type".to_string(), "application/json".to_string()),
        ];
        let url = format!("{}{uri_path}", self.base());
        let resp = map_status(self.transport.post_json(&url, &headers, body).await)?;
        serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)
    }

    /// Initialize a hosted Iyzico Checkout Form crediting `account_id` for
    /// `amount_micros` (on-ramp). `callback_url` MUST be Mint's public
    /// `/v1/deposits/iyzico/webhook` route: Iyzico returns the buyer's browser
    /// there with the form `token`. The account rides in `basketId` (and
    /// `conversationId`) and is read back from the authenticated detail call.
    /// Returns the hosted `paymentPageUrl` + form `token`.
    ///
    /// Env-key gated: requires an `api_key` + `extra("secret_key")`; missing the
    /// secret key ⇒ [`RampError::ProviderUnavailable`] WITHOUT any network call.
    pub async fn initialize_checkout(
        &self,
        account_id: &str,
        amount_micros: i64,
        callback_url: &str,
    ) -> Result<Checkout, RampError> {
        if self.secret_key().is_none() || callback_url.trim().is_empty() {
            return Err(RampError::ProviderUnavailable);
        }
        let price = micros_to_dollars(amount_micros);
        let body = serde_json::json!({
            "locale": "en",
            "conversationId": account_id,
            "price": price,
            "paidPrice": price,
            "currency": DEPOSIT_CURRENCY,
            "basketId": account_id,
            "paymentGroup": "PRODUCT",
            "callbackUrl": callback_url,
            "enabledInstallments": [1],
            "basketItems": [{
                "id": "AIT",
                "name": "AIT deposit",
                "category1": "Credits",
                "itemType": "VIRTUAL",
                "price": price,
            }],
        });
        let raw = serde_json::to_vec(&body).expect("json");
        let v = self
            .signed_post("/payment/iyzipos/checkoutform/initialize/auth/ecom", &raw)
            .await?;
        if v["status"].as_str() != Some("success") {
            tracing::warn!(error = ?v["errorMessage"], "iyzico checkout initialize refused");
            return Err(RampError::ProviderUnavailable);
        }
        let checkout_url = v["paymentPageUrl"]
            .as_str()
            .ok_or(RampError::ProviderUnavailable)?
            .to_string();
        let checkout_id = v["token"]
            .as_str()
            .ok_or(RampError::ProviderUnavailable)?
            .to_string();
        Ok(Checkout {
            checkout_url,
            checkout_id,
        })
    }

    /// Resolve a Checkout Form `token` (from the browser callback) into the
    /// settled payment via the signed checkout-form detail call.
    async fn retrieve_checkout(&self, token: &str) -> Result<SettledPayment, RampError> {
        let body = serde_json::json!({ "locale": "en", "token": token });
        let raw = serde_json::to_vec(&body).expect("json");
        let v = self
            .signed_post("/payment/iyzipos/checkoutform/auth/ecom/detail", &raw)
            .await?;
        settled_payment(&v)
    }

    /// The signed `/payment/detail` record for a `paymentId`.
    async fn payment_detail(&self, payment_id: &str) -> Result<Value, RampError> {
        let body = serde_json::json!({ "locale": "en", "paymentId": payment_id });
        let raw = serde_json::to_vec(&body).expect("json");
        self.signed_post("/payment/detail", &raw).await
    }

    /// Verify + classify an inbound delivery on `/v1/deposits/iyzico/webhook`:
    ///
    /// - a JSON merchant notification carrying `X-IYZ-SIGNATURE-V3` (payment
    ///   success ⇒ deposit; refund / cancel / chargeback ⇒ reversal);
    /// - the buyer-browser Checkout Form callback (form POST with `token`),
    ///   resolved through the signed checkout-form detail call;
    /// - the legacy 3DS callback (form POST with `paymentId`), re-verified
    ///   downstream by [`Ramp::confirm_deposit`].
    pub async fn verify_event<H: Headers>(
        &self,
        headers: &H,
        raw: &[u8],
    ) -> Result<VerifiedEvent, WebhookError> {
        let secret = self
            .secret_key()
            .ok_or(WebhookError::NotConfigured)?
            .to_string();
        if let Some(sig) = headers.get_header("x-iyz-signature-v3") {
            return self.verify_notification(&secret, sig, raw).await;
        }
        let form = parse_form(raw);
        let get = |name: &str| {
            form.iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.clone())
                .filter(|v| !v.is_empty())
        };
        if let Some(token) = get("token") {
            if get("status").as_deref() == Some("failure") {
                return Err(WebhookError::Ignored("failure".into()));
            }
            let paid = match self.retrieve_checkout(&token).await {
                Ok(p) => p,
                Err(RampError::NotConfirmed) => {
                    return Err(WebhookError::Ignored("checkout not paid".into()))
                }
                Err(RampError::DepositNotFound) => {
                    return Err(WebhookError::Malformed("unknown checkout token".into()))
                }
                // Transient: surfaced as a non-2xx so the caller shows a retry
                // page; the merchant notification will still credit.
                Err(_) => return Err(WebhookError::Malformed("iyzico unavailable".into())),
            };
            let account_id = paid
                .account_id
                .ok_or_else(|| WebhookError::Malformed("payment has no basketId".into()))?;
            return Ok(VerifiedEvent::Deposit(DepositEvent {
                provider: ProviderId::Iyzico,
                deposit_ref: paid.payment_id,
                amount_micros: paid.amount_micros,
                asset: Asset::Usd,
                account_id,
            }));
        }
        verify_webhook(&self.cfg, headers, raw).map(VerifiedEvent::Deposit)
    }

    /// Verify an `X-IYZ-SIGNATURE-V3` merchant notification and classify it.
    async fn verify_notification(
        &self,
        secret: &str,
        sig: &str,
        raw: &[u8],
    ) -> Result<VerifiedEvent, WebhookError> {
        let v: Value =
            serde_json::from_slice(raw).map_err(|e| WebhookError::Malformed(e.to_string()))?;
        let text = |k: &str| match &v[k] {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => String::new(),
        };
        let event_type = text("iyziEventType");
        let payment_id = {
            let p = text("paymentId");
            if p.is_empty() {
                text("iyziPaymentId")
            } else {
                p
            }
        };
        let conversation = text("paymentConversationId");
        let status = text("status");
        let token = text("token");
        // Iyzico signs `secretKey + iyziEventType + paymentId + paymentConversationId
        // + status` for API payments, with the checkout `token` inserted after the
        // payment id for hosted (Checkout Form) payments.
        let message = if token.is_empty() {
            format!("{secret}{event_type}{payment_id}{conversation}{status}")
        } else {
            format!("{secret}{event_type}{payment_id}{token}{conversation}{status}")
        };
        hmac_sha256_hex_verify(secret.as_bytes(), message.as_bytes(), sig)?;
        if payment_id.is_empty() {
            return Err(WebhookError::Ignored(format!("{event_type} without paymentId")));
        }

        let upper = event_type.to_ascii_uppercase();
        let reversal = if upper.contains("CHARGEBACK") || upper.contains("DISPUTE") {
            Some(ReversalReason::Dispute)
        } else if upper.contains("REFUND") || upper.contains("CANCEL") {
            Some(ReversalReason::Refund)
        } else {
            None
        };
        if let Some(reason) = reversal {
            if !status.is_empty() && !status.eq_ignore_ascii_case("SUCCESS") {
                return Err(WebhookError::Ignored(format!("{event_type} {status}")));
            }
            // A CANCEL voids the whole payment; a refund notification carries the
            // refunded amount when Iyzico includes one, otherwise the whole
            // minted deposit is clawed back.
            let amount_micros = if upper.contains("CANCEL") {
                None
            } else {
                ["refundPrice", "refundedPrice", "price", "paidPrice"]
                    .iter()
                    .find_map(|k| money(&v[*k]))
                    .map(i64::abs)
                    .filter(|&m| m > 0)
            };
            return Ok(VerifiedEvent::Reversal(ReversalEvent {
                provider: ProviderId::Iyzico,
                deposit_ref: payment_id,
                alt_refs: Vec::new(),
                amount_micros,
                reason,
            }));
        }
        if !status.eq_ignore_ascii_case("SUCCESS") {
            return Err(WebhookError::Ignored(format!("{event_type} {status}")));
        }
        // A successful payment: resolve the authoritative amount + account from
        // the signed /payment/detail (the notification carries neither).
        let detail = self
            .payment_detail(&payment_id)
            .await
            .map_err(|_| WebhookError::Malformed("iyzico payment detail unavailable".into()))?;
        let paid = match settled_payment(&detail) {
            Ok(p) => p,
            Err(_) => return Err(WebhookError::Ignored("payment not settled".into())),
        };
        let account_id = paid
            .account_id
            .ok_or_else(|| WebhookError::Ignored("payment was not created by Mint".into()))?;
        Ok(VerifiedEvent::Deposit(DepositEvent {
            provider: ProviderId::Iyzico,
            deposit_ref: paid.payment_id,
            amount_micros: paid.amount_micros,
            asset: Asset::Usd,
            account_id,
        }))
    }
}

/// Split an Iyzico payout ref (`<paymentId>` or `<paymentId>:<suffix>`) into
/// the ORIGINAL payment id it refunds.
fn refund_payment_id(payout_ref: &str) -> &str {
    payout_ref.split(':').next().unwrap_or("").trim()
}

#[async_trait]
impl Ramp for IyzicoRamp {
    async fn confirm_deposit(&self, deposit_ref: &str) -> Result<Deposit, RampError> {
        // `deposit_ref` is the Iyzico paymentId. Re-query /payment/detail.
        let v = self.payment_detail(deposit_ref).await?;
        let paid = settled_payment(&v)?;
        if paid.payment_id != deposit_ref {
            return Err(RampError::NotConfirmed);
        }
        // Bind the credit to the payment: the account is the `basketId` set at
        // checkout time, read from the AUTHENTICATED /payment/detail response,
        // NOT from the unauthenticated callback form. `mint` rejects the deposit
        // unless it equals the credited account, so a forged callback cannot
        // redirect a real payment; a payment Mint did not create (no basketId)
        // cannot be credited at all.
        let bound_account = Some(paid.account_id.ok_or(RampError::NotConfirmed)?);
        Ok(Deposit {
            deposit_ref: deposit_ref.to_string(),
            amount_micros: Micros::new(paid.amount_micros),
            asset: Asset::Usd,
            confirmed: true,
            bound_account,
        })
    }

    async fn initiate_payout(
        &self,
        payout_ref: &str,
        _dest: &PayoutDest,
        amount: Micros,
    ) -> Result<Payout, RampError> {
        // Look up the original payment's item-level transaction + currency.
        let payment_id = refund_payment_id(payout_ref);
        let detail = self.payment_detail(payment_id).await?;
        if detail["status"].as_str() != Some("success") {
            return Err(RampError::ProviderUnavailable);
        }
        let currency = detail["currency"].as_str().unwrap_or(DEPOSIT_CURRENCY);
        if !currency.eq_ignore_ascii_case(DEPOSIT_CURRENCY) {
            // A USD burn cannot be refunded as a different currency 1:1.
            return Err(RampError::ProviderUnavailable);
        }
        let item = detail["itemTransactions"]
            .as_array()
            .and_then(|items| {
                items.iter().find(|it| {
                    money(&it["paidPrice"]).is_some_and(|p| p >= amount.get())
                        && !it["paymentTransactionId"].is_null()
                })
            })
            .ok_or(RampError::ProviderUnavailable)?;
        let transaction_id = match &item["paymentTransactionId"] {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            _ => return Err(RampError::ProviderUnavailable),
        };
        let body = serde_json::json!({
            "locale": "en",
            "conversationId": payout_ref,
            "paymentTransactionId": transaction_id,
            "price": micros_to_dollars(amount.get()),
            "currency": currency,
        });
        let raw = serde_json::to_vec(&body).expect("json");
        let v = self.signed_post("/payment/refund", &raw).await?;
        if v["status"].as_str() != Some("success") {
            tracing::warn!(error = ?v["errorMessage"], "iyzico refund refused");
            return Err(RampError::ProviderUnavailable);
        }
        Ok(Payout {
            payout_ref: payout_ref.to_string(),
            amount_micros: amount,
            status: PayoutStatus::Paid,
            provider_ref: Some(transaction_id),
        })
    }

    async fn check_payout(
        &self,
        payout_ref: &str,
        _dest: &PayoutDest,
        amount: Micros,
    ) -> Result<(), RampError> {
        // An Iyzico payout is a refund against the ORIGINAL card payment named
        // by the ref (`<paymentId>[:<suffix>]`, Iyzico payment ids are numeric);
        // it must be a whole number of cents.
        let payment_id = refund_payment_id(payout_ref);
        if payment_id.is_empty()
            || !payment_id.bytes().all(|b| b.is_ascii_digit())
            || !crate::card::is_whole_cents(amount.get())
        {
            return Err(RampError::Unsupported);
        }
        Ok(())
    }
}

/// Parse a legacy Iyzico 3DS callback (form-encoded, `paymentId` +
/// `conversationId`). This form is UNauthenticated, so nothing it carries is
/// trusted for crediting: authenticity, the authoritative amount, AND the
/// authoritative account are all established by the subsequent IYZWSv2-signed
/// `/payment/detail` re-query in `confirm_deposit`, which binds the mint to the
/// `basketId` that actually paid (see [`Deposit::bound_account`]). The
/// `account_id` extracted here is only a routing hint; `mint` rejects it if it
/// does not match the payment's real account.
pub fn verify_webhook<H: Headers>(
    cfg: &ProviderConfig,
    _headers: &H,
    raw: &[u8],
) -> Result<DepositEvent, WebhookError> {
    // Iyzico must be configured (secret key) to re-verify downstream.
    if cfg.extra("secret_key").filter(|k| !k.is_empty()).is_none() {
        return Err(WebhookError::NotConfigured);
    }
    let form = parse_form(raw);
    let get = |name: &str| form.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    let status = get("status").unwrap_or_default();
    if status == "failure" {
        return Err(WebhookError::Ignored("failure".into()));
    }
    let payment_id =
        get("paymentId").ok_or_else(|| WebhookError::Malformed("missing paymentId".into()))?;
    let account_id = get("conversationId")
        .ok_or_else(|| WebhookError::Malformed("missing conversationId".into()))?;
    let amount_micros = get("paidPrice")
        .and_then(|p| decimal_to_micros(&p))
        .unwrap_or(0);
    Ok(DepositEvent {
        provider: ProviderId::Iyzico,
        deposit_ref: payment_id,
        amount_micros,
        asset: Asset::Usd,
        account_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto_tr::test_support::FakeTransport;
    use std::collections::HashMap;

    fn cfg(secret_key: Option<&str>) -> ProviderConfig {
        let mut extra = HashMap::new();
        if let Some(k) = secret_key {
            extra.insert("secret_key".to_string(), k.to_string());
        }
        ProviderConfig {
            provider: ProviderId::Iyzico,
            api_key: Some("iyz_api_key".into()),
            webhook_secret: Some("iyz-wh".into()),
            mode: Mode::Sandbox,
            extra,
        }
    }

    // ---- on-ramp: real IYZWSv2-signed checkout-form initialize --------------

    #[tokio::test]
    async fn initialize_checkout_returns_payment_page_url_and_token() {
        let transport = FakeTransport::new()
            .route(
                "/payment/iyzipos/checkoutform/initialize/auth/ecom",
                200,
                serde_json::json!({
                    "status":"success",
                    "token":"tok_abc",
                    "paymentPageUrl":"https://sandbox-api.iyzipay.com/pay/tok_abc"
                }),
            )
            .arc();
        let ramp = IyzicoRamp::new(transport.clone(), cfg(Some("sk")));
        let checkout = ramp
            .initialize_checkout("acct_tr", 30_000_000, "https://mint.example/v1/deposits/iyzico/webhook")
            .await
            .expect("initialized");
        assert_eq!(checkout.checkout_id, "tok_abc");
        assert_eq!(
            checkout.checkout_url,
            "https://sandbox-api.iyzipay.com/pay/tok_abc"
        );
        let body = String::from_utf8(transport.last_post_body()).unwrap();
        assert!(
            body.contains("\"conversationId\":\"acct_tr\""),
            "body: {body}"
        );
        assert!(body.contains("\"price\":\"30.00\""), "body: {body}");
    }

    #[tokio::test]
    async fn initialize_checkout_without_secret_key_is_inactive() {
        let ramp = IyzicoRamp::new(FakeTransport::new().arc(), cfg(None));
        assert_eq!(
            ramp.initialize_checkout("acct_tr", 30_000_000, "https://mint.example/cb").await,
            Err(RampError::ProviderUnavailable)
        );
    }

    #[tokio::test]
    async fn initialize_checkout_provider_failure_status_is_unavailable() {
        let transport = FakeTransport::new()
            .route(
                "/payment/iyzipos/checkoutform/initialize/auth/ecom",
                200,
                serde_json::json!({"status":"failure","errorMessage":"invalid signature"}),
            )
            .arc();
        let ramp = IyzicoRamp::new(transport, cfg(Some("sk")));
        assert_eq!(
            ramp.initialize_checkout("acct_tr", 30_000_000, "https://mint.example/cb").await,
            Err(RampError::ProviderUnavailable)
        );
    }

    // ---- webhook (3DS callback) parse + downstream re-verify -----------------

    #[test]
    fn callback_parses_payment_id_and_exact_amount() {
        let body = b"status=success&paymentId=pay_42&conversationId=acct_tr&paidPrice=30.00";
        let hdrs: HashMap<String, String> = HashMap::new();
        let ev = verify_webhook(&cfg(Some("sk")), &hdrs, body).expect("parsed");
        assert_eq!(ev.provider, ProviderId::Iyzico);
        assert_eq!(ev.deposit_ref, "pay_42");
        assert_eq!(ev.account_id, "acct_tr");
        assert_eq!(ev.amount_micros, 30_000_000);
        assert_eq!(ev.asset, Asset::Usd);
        assert_eq!(ev.namespaced_ref(), "iyzico:pay_42");
    }

    #[test]
    fn callback_without_secret_key_is_not_configured() {
        let body = b"status=success&paymentId=pay_42&conversationId=acct_tr";
        let hdrs: HashMap<String, String> = HashMap::new();
        assert_eq!(
            verify_webhook(&cfg(None), &hdrs, body),
            Err(WebhookError::NotConfigured)
        );
    }

    #[test]
    fn callback_failure_status_is_ignored() {
        let body = b"status=failure&paymentId=pay_42&conversationId=acct_tr";
        let hdrs: HashMap<String, String> = HashMap::new();
        assert_eq!(
            verify_webhook(&cfg(Some("sk")), &hdrs, body),
            Err(WebhookError::Ignored("failure".into()))
        );
    }

    #[tokio::test]
    async fn confirm_reverifies_via_payment_detail_then_credits_exact() {
        let transport = FakeTransport::new()
            .route(
                "/payment/detail",
                200,
                serde_json::json!({"status":"success","paymentStatus":"SUCCESS","paymentId":"pay_42","currency":"USD","paidPrice":"30.00","conversationId":"pay_42","basketId":"acct_tr"}),
            )
            .arc();
        let ramp = IyzicoRamp::new(transport, cfg(Some("sk")));
        let dep = ramp.confirm_deposit("pay_42").await.expect("confirmed");
        assert!(dep.confirmed);
        assert_eq!(dep.amount_micros, Micros::new(30_000_000));
        assert_eq!(dep.asset, Asset::Usd);
        // The credited account is bound to the AUTHENTICATED /payment/detail
        // response, so `mint` can reject a callback that names a different
        // account than the one that actually paid.
        assert_eq!(dep.bound_account.as_deref(), Some("acct_tr"));

        // A callback that never actually settled: /payment/detail says not-success.
        let transport = FakeTransport::new()
            .route(
                "/payment/detail",
                200,
                serde_json::json!({"status":"success","paymentStatus":"FAILURE"}),
            )
            .arc();
        let ramp = IyzicoRamp::new(transport, cfg(Some("sk")));
        assert_eq!(
            ramp.confirm_deposit("pay_42").await,
            Err(RampError::NotConfirmed)
        );

        // Provider outage → retryable.
        let ramp = IyzicoRamp::new(FakeTransport::failing(), cfg(Some("sk")));
        assert_eq!(
            ramp.confirm_deposit("pay_42").await,
            Err(RampError::ProviderUnavailable)
        );
    }

    #[tokio::test]
    async fn refund_payout_pays() {
        let transport = FakeTransport::new()
            .route(
                "/payment/detail",
                200,
                serde_json::json!({"status":"success","currency":"USD","itemTransactions":[{"paymentTransactionId":"7771","paidPrice":"30.00"}]}),
            )
            .route(
                "/payment/refund",
                200,
                serde_json::json!({"status":"success"}),
            )
            .arc();
        let ramp = IyzicoRamp::new(transport.clone(), cfg(Some("sk")));
        let dest = PayoutDest::Stripe {
            account: "n/a".into(),
        };
        let payout = ramp
            .initiate_payout("12345", &dest, Micros::new(10_000_000))
            .await
            .expect("paid");
        assert_eq!(payout.status, PayoutStatus::Paid);
        assert_eq!(payout.amount_micros, Micros::new(10_000_000));
        // The refund targets the ORIGINAL item transaction, not the Mint ref.
        let body = String::from_utf8(transport.last_post_body()).unwrap();
        assert!(body.contains("\"paymentTransactionId\":\"7771\""), "body: {body}");
        assert!(body.contains("\"currency\":\"USD\""), "body: {body}");
    }

    #[tokio::test]
    async fn check_payout_requires_numeric_original_payment_id() {
        let ramp = IyzicoRamp::new(FakeTransport::new().arc(), cfg(Some("sk")));
        let dest = PayoutDest::Stripe { account: "n/a".into() };
        assert!(ramp.check_payout("12345:r1", &dest, Micros::new(10_000)).await.is_ok());
        assert_eq!(
            ramp.check_payout("anything", &dest, Micros::new(10_000)).await,
            Err(RampError::Unsupported)
        );
    }

    #[tokio::test]
    async fn browser_callback_token_resolves_payment_and_basket_account() {
        let transport = FakeTransport::new()
            .route(
                "/checkoutform/auth/ecom/detail",
                200,
                serde_json::json!({"status":"success","paymentStatus":"SUCCESS","paymentId":"9001","paidPrice":"30.00","currency":"USD","basketId":"acct_tr","token":"tok_abc"}),
            )
            .arc();
        let ramp = IyzicoRamp::new(transport, cfg(Some("sk")));
        let hdrs: HashMap<String, String> = HashMap::new();
        let ev = ramp.verify_event(&hdrs, b"token=tok_abc").await.expect("verified");
        match ev {
            VerifiedEvent::Deposit(d) => {
                assert_eq!(d.deposit_ref, "9001");
                assert_eq!(d.account_id, "acct_tr");
                assert_eq!(d.amount_micros, 30_000_000);
            }
            other => panic!("expected deposit, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn signed_refund_notification_is_a_reversal() {
        let secret = "sk";
        let (event, pid, conv, status) = ("REFUND", "9001", "acct_tr", "SUCCESS");
        let msg = format!("{secret}{event}{pid}{conv}{status}");
        let sig = crate::crypto_tr::test_support::hmac_hex(secret.as_bytes(), msg.as_bytes());
        let body = serde_json::json!({"iyziEventType":event,"paymentId":pid,"paymentConversationId":conv,"status":status}).to_string();
        let mut hdrs: HashMap<String, String> = HashMap::new();
        hdrs.insert("x-iyz-signature-v3".into(), sig);
        let ramp = IyzicoRamp::new(FakeTransport::new().arc(), cfg(Some(secret)));
        match ramp.verify_event(&hdrs, body.as_bytes()).await.expect("verified") {
            VerifiedEvent::Reversal(r) => {
                assert_eq!(r.deposit_ref, "9001");
                assert_eq!(r.reason, ReversalReason::Refund);
            }
            other => panic!("expected reversal, got {other:?}"),
        }
        hdrs.insert("x-iyz-signature-v3".into(), "00".into());
        assert_eq!(
            ramp.verify_event(&hdrs, body.as_bytes()).await,
            Err(WebhookError::BadSignature)
        );
    }
}
