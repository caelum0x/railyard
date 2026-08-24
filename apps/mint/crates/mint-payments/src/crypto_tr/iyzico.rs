//! Iyzico adapter (Turkish card acquirer). On-ramp: initialize a 3DS checkout;
//! the 3DS callback POSTs `conversationId`/`paymentId` form fields, which are
//! re-verified against `/payment/detail` (IYZWSv2 auth) BEFORE crediting.
//! Off-ramp: refund. `asset = Usd`.

use std::sync::Arc;

use async_trait::async_trait;
use mint_core::Micros;
use mint_reserve::{Asset, Deposit, Payout, PayoutDest, PayoutStatus, Ramp, RampError};
use serde_json::Value;

use crate::card::Checkout;
use crate::config::{Mode, ProviderConfig};
use crate::crypto_tr::{micros_to_dollars, parse_form};
use crate::event::{DepositEvent, WebhookError};
use crate::http::{map_status, Headers, HttpTransport};
use crate::money::decimal_to_micros;
use crate::provider::ProviderId;
use crate::verify::iyzws_v2_sign;

fn api_base(mode: Mode) -> &'static str {
    match mode {
        Mode::Sandbox => "https://sandbox-api.iyzipay.com",
        Mode::Live => "https://api.iyzipay.com",
    }
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

    /// Sign + POST an IYZWSv2-authenticated request to `uri_path`.
    async fn signed_post(&self, uri_path: &str, body: &[u8]) -> Result<Value, RampError> {
        let api_key = self.cfg.api_key.clone().unwrap_or_default();
        let secret_key = self.cfg.extra("secret_key").unwrap_or_default().to_string();
        let random_key = format!("{}", body.len()); // deterministic per-body nonce
        let authz = iyzws_v2_sign(&api_key, &secret_key, &random_key, uri_path, body);
        let headers = vec![
            ("authorization".to_string(), authz),
            ("content-type".to_string(), "application/json".to_string()),
        ];
        let url = format!("{}{uri_path}", self.base());
        let resp = map_status(self.transport.post_json(&url, &headers, body).await)?;
        serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)
    }

    /// Initialize a hosted Iyzico Checkout Form crediting `account_id` for
    /// `amount_micros` (on-ramp). Signs + POSTs a real
    /// `/payment/iyzipos/checkoutform/initialize/auth/ecom` request (IYZWSv2)
    /// with `conversationId = account_id` (echoed back on the 3DS callback so the
    /// mint target resolves). Returns the hosted `paymentPageUrl` + form `token`.
    ///
    /// Env-key gated: requires an `api_key` + `extra("secret_key")`; missing the
    /// secret key ⇒ [`RampError::ProviderUnavailable`] WITHOUT any network call.
    pub async fn initialize_checkout(
        &self,
        account_id: &str,
        amount_micros: i64,
    ) -> Result<Checkout, RampError> {
        if self
            .cfg
            .extra("secret_key")
            .filter(|k| !k.is_empty())
            .is_none()
        {
            return Err(RampError::ProviderUnavailable);
        }
        let price = micros_to_dollars(amount_micros);
        let body = serde_json::json!({
            "locale": "en",
            "conversationId": account_id,
            "price": price,
            "paidPrice": price,
            "currency": "USD",
            "basketId": account_id,
            "paymentGroup": "PRODUCT",
            "callbackUrl": "https://mint.local/webhooks/iyzico",
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
}

#[async_trait]
impl Ramp for IyzicoRamp {
    async fn confirm_deposit(&self, deposit_ref: &str) -> Result<Deposit, RampError> {
        // `deposit_ref` is the Iyzico paymentId. Re-query /payment/detail.
        let body = serde_json::json!({
            "locale": "en",
            "conversationId": deposit_ref,
            "paymentId": deposit_ref,
        });
        let raw = serde_json::to_vec(&body).expect("json");
        let v = self.signed_post("/payment/detail", &raw).await?;
        let status = v["status"].as_str().unwrap_or("");
        let payment_status = v["paymentStatus"].as_str().unwrap_or("");
        let confirmed = status == "success" && payment_status == "SUCCESS";
        if !confirmed {
            return Err(RampError::NotConfirmed);
        }
        // Bind the credit to the payment: the account is taken from the
        // AUTHENTICATED /payment/detail response (the `conversationId`/`basketId`
        // set at checkout time), NOT from the unauthenticated 3DS callback form.
        // `mint` rejects the deposit unless this equals the credited account, so a
        // forged callback cannot redirect a real payment to another account.
        let bound_account = v["conversationId"]
            .as_str()
            .or_else(|| v["basketId"].as_str())
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string());
        // The authoritative amount is the confirmed `paidPrice` from
        // /payment/detail, never the callback's self-reported value.
        let micros = v["paidPrice"]
            .as_str()
            .and_then(decimal_to_micros)
            .ok_or(RampError::ProviderUnavailable)?;
        Ok(Deposit {
            deposit_ref: deposit_ref.to_string(),
            amount_micros: Micros::new(micros),
            asset: Asset::Usd,
            confirmed,
            bound_account,
        })
    }

    async fn initiate_payout(
        &self,
        payout_ref: &str,
        _dest: &PayoutDest,
        amount: Micros,
    ) -> Result<Payout, RampError> {
        let dollars = format!(
            "{}.{:02}",
            amount.get() / 1_000_000,
            (amount.get() % 1_000_000) / 10_000
        );
        let body = serde_json::json!({
            "locale": "en",
            "conversationId": payout_ref,
            "paymentTransactionId": payout_ref,
            "price": dollars,
            "currency": "USD",
        });
        let raw = serde_json::to_vec(&body).expect("json");
        let v = self.signed_post("/payment/refund", &raw).await?;
        if v["status"].as_str() != Some("success") {
            return Err(RampError::ProviderUnavailable);
        }
        Ok(Payout {
            payout_ref: payout_ref.to_string(),
            amount_micros: amount,
            status: PayoutStatus::Paid,
        })
    }
}

/// Parse an Iyzico 3DS callback (form-encoded). This form is UNauthenticated, so
/// nothing it carries is trusted for crediting: authenticity, the authoritative
/// amount, AND the authoritative account are all established by the subsequent
/// IYZWSv2-signed `/payment/detail` re-query in `confirm_deposit`, which binds
/// the mint to the `conversationId`/`basketId` that actually paid (see
/// [`Deposit::bound_account`]). The `account_id` extracted here is only a routing
/// hint for which Mint account to attempt; `mint` rejects it if it does not match
/// the payment's real account.
pub fn verify_webhook<H: Headers>(
    cfg: &ProviderConfig,
    _headers: &H,
    raw: &[u8],
) -> Result<DepositEvent, WebhookError> {
    // Iyzico must be configured (secret key) to re-verify downstream.
    if cfg.extra("secret_key").is_none() {
        return Err(WebhookError::NotConfigured);
    }
    let form = parse_form(raw);
    let get = |name: &str| form.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
    let payment_id =
        get("paymentId").ok_or_else(|| WebhookError::Malformed("missing paymentId".into()))?;
    let account_id = get("conversationId")
        .ok_or_else(|| WebhookError::Malformed("missing conversationId".into()))?;
    let status = get("status").unwrap_or_default();
    if status == "failure" {
        return Err(WebhookError::Ignored("failure".into()));
    }
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
            .initialize_checkout("acct_tr", 30_000_000)
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
            ramp.initialize_checkout("acct_tr", 30_000_000).await,
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
            ramp.initialize_checkout("acct_tr", 30_000_000).await,
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
                serde_json::json!({"status":"success","paymentStatus":"SUCCESS","paidPrice":"30.00","conversationId":"acct_tr","basketId":"acct_tr"}),
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
                "/payment/refund",
                200,
                serde_json::json!({"status":"success"}),
            )
            .arc();
        let ramp = IyzicoRamp::new(transport, cfg(Some("sk")));
        let dest = PayoutDest::Stripe {
            account: "n/a".into(),
        };
        let payout = ramp
            .initiate_payout("iyzico:pay_42", &dest, Micros::new(10_000_000))
            .await
            .expect("paid");
        assert_eq!(payout.status, PayoutStatus::Paid);
        assert_eq!(payout.amount_micros, Micros::new(10_000_000));
    }
}
