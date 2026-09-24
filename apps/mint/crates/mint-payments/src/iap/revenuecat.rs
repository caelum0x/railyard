//! RevenueCat adapter (mobile IAP, webhook-only). Webhook authenticity: the
//! `Authorization` header must EXACTLY equal the configured shared secret,
//! compared in constant time (NOT an HMAC). Events `INITIAL_PURCHASE` /
//! `RENEWAL` / `NON_RENEWING_PURCHASE`; `app_user_id` → `account_id`, the
//! USD-normalized `price` → µ$ (never `price_in_purchased_currency`, which is
//! in the store's LOCAL currency). No server checkout, no payout.
//!
//! Re-confirmation: RevenueCat's REST API is keyed on the PAYER
//! (`GET /v1/subscribers/{app_user_id}`), not on a transaction, so the reserve
//! calls [`Ramp::confirm_deposit_for`] with the account being credited. The
//! adapter finds the store transaction under that subscriber's
//! `non_subscriptions` / `subscriptions`, fails closed on a refund, and binds
//! the deposit to that account. RevenueCat's customer record does not carry a
//! per-transaction USD amount, so the USD amount is the one from the
//! constant-time-authenticated webhook; when the subscription record DOES
//! report a USD price it caps the mint.

use std::sync::Arc;

use async_trait::async_trait;
use mint_core::Micros;
use mint_reserve::{Asset, Deposit, Payout, PayoutDest, Ramp, RampError};
use serde_json::Value;

use crate::config::ProviderConfig;
use crate::event::{DepositEvent, ReversalEvent, ReversalReason, WebhookError};
use crate::http::{encode_component, map_status, Headers, HttpTransport};
use crate::iap::find_transaction;
use crate::money::json_dollars_to_micros;
use crate::provider::ProviderId;
use crate::verify::const_eq_secret;

const API_BASE: &str = "https://api.revenuecat.com";

/// The record keys RevenueCat uses for a purchase's store transaction id.
const TXN_KEYS: [&str; 3] = ["store_transaction_id", "id", "transaction_id"];

/// A live RevenueCat [`Ramp`] over an injected [`HttpTransport`].
pub struct RevenueCatRamp {
    transport: Arc<dyn HttpTransport>,
    cfg: ProviderConfig,
}

impl RevenueCatRamp {
    /// Build the adapter from its transport + resolved config.
    pub fn new(transport: Arc<dyn HttpTransport>, cfg: ProviderConfig) -> Self {
        RevenueCatRamp { transport, cfg }
    }

    fn auth_headers(&self) -> Vec<(String, String)> {
        let key = self.cfg.api_key.clone().unwrap_or_default();
        vec![("authorization".into(), format!("Bearer {key}"))]
    }

    /// Fetch `app_user_id`'s subscriber record and locate `transaction_id` in it.
    async fn lookup(&self, app_user_id: &str, transaction_id: &str) -> Result<Value, RampError> {
        if app_user_id.trim().is_empty() || transaction_id.trim().is_empty() {
            return Err(RampError::DepositNotFound);
        }
        let url = format!(
            "{API_BASE}/v1/subscribers/{}",
            encode_component(app_user_id)
        );
        let resp = map_status(self.transport.get(&url, &self.auth_headers()).await)?;
        let v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        let subscriber = &v["subscriber"];
        find_transaction(
            &[&subscriber["non_subscriptions"], &subscriber["subscriptions"]],
            &TXN_KEYS,
            transaction_id,
        )
        .cloned()
        // GET /v1/subscribers creates an EMPTY subscriber for an unknown id,
        // so "no such transaction for this payer" is the not-found signal.
        .ok_or(RampError::DepositNotFound)
    }
}

#[async_trait]
impl Ramp for RevenueCatRamp {
    async fn confirm_deposit(&self, _deposit_ref: &str) -> Result<Deposit, RampError> {
        // A transaction id alone cannot be looked up in RevenueCat (the API is
        // keyed on app_user_id); the reserve always uses `confirm_deposit_for`.
        Err(RampError::DepositNotFound)
    }

    async fn confirm_deposit_for(
        &self,
        deposit_ref: &str,
        account: &str,
        amount: Micros,
    ) -> Result<Deposit, RampError> {
        let record = self.lookup(account, deposit_ref).await?;
        // Fail closed on a refunded/revoked purchase: a later refund or chargeback
        // must NOT mint.
        if is_refunded(&record) {
            return Err(RampError::NotConfirmed);
        }
        if amount.get() <= 0 {
            return Err(RampError::NotConfirmed);
        }
        // A USD price on the record (subscriptions carry `price.amount` +
        // `price.currency`) is authoritative and caps the mint; otherwise the
        // authenticated webhook's USD amount stands.
        let confirmed = match usd_price(&record) {
            Some(p) if p > 0 => p,
            _ => amount.get(),
        };
        Ok(Deposit {
            deposit_ref: deposit_ref.to_string(),
            amount_micros: Micros::new(confirmed),
            asset: Asset::Usd,
            confirmed: true,
            bound_account: Some(account.to_string()),
        })
    }

    async fn initiate_payout(
        &self,
        _payout_ref: &str,
        _dest: &PayoutDest,
        _amount: Micros,
    ) -> Result<Payout, RampError> {
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

/// The record's price in µ$ when (and only when) RevenueCat reports it in USD.
fn usd_price(record: &Value) -> Option<i64> {
    let price = &record["price"];
    let currency = price["currency"].as_str()?;
    if !currency.eq_ignore_ascii_case("USD") {
        return None;
    }
    json_dollars_to_micros(&price["amount"])
}

/// Whether a RevenueCat purchase/subscriber payload indicates the transaction
/// was refunded or revoked, in which case it must not mint. Recognizes a
/// non-null `refunded_at`, an explicit `is_refunded: true`, and a revoked
/// entitlement flag.
fn is_refunded(v: &Value) -> bool {
    !v["refunded_at"].is_null()
        || v["is_refunded"].as_bool() == Some(true)
        || v["is_revoked"].as_bool() == Some(true)
}

/// Verify a RevenueCat webhook: constant-time `Authorization` equality over the
/// shared secret, then extract the purchase.
pub fn verify_webhook<H: Headers>(
    cfg: &ProviderConfig,
    headers: &H,
    raw: &[u8],
) -> Result<DepositEvent, WebhookError> {
    verify_authorization(cfg, headers)?;

    let v: Value =
        serde_json::from_slice(raw).map_err(|e| WebhookError::Malformed(e.to_string()))?;
    let event = &v["event"];
    let event_type = event["type"].as_str().unwrap_or("");
    if !matches!(
        event_type,
        "INITIAL_PURCHASE" | "RENEWAL" | "NON_RENEWING_PURCHASE"
    ) {
        return Err(WebhookError::Ignored(event_type.to_string()));
    }
    // Credit under the store TRANSACTION id (stable per purchase and repeated
    // on the later refund event), falling back to the event id only when the
    // store did not report one, so a refund can be tied back to its deposit.
    let deposit_ref = event["transaction_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| event["id"].as_str())
        .ok_or_else(|| WebhookError::Malformed("missing event id".into()))?
        .to_string();
    // `price` is RevenueCat's USD-normalized price; `price_in_purchased_currency`
    // is in the storefront's local currency (TRY, EUR, …) and must never be
    // minted 1:1 as USD.
    let amount_micros = json_dollars_to_micros(&event["price"])
        .filter(|&m| m > 0)
        .ok_or_else(|| WebhookError::Malformed("missing or non-positive USD price".into()))?;
    let account_id = event["app_user_id"]
        .as_str()
        .ok_or_else(|| WebhookError::Malformed("missing app_user_id".into()))?
        .to_string();
    Ok(DepositEvent {
        provider: ProviderId::RevenueCat,
        deposit_ref,
        amount_micros,
        asset: Asset::Usd,
        account_id,
    })
}

/// Constant-time `Authorization` equality over the configured shared secret.
fn verify_authorization<H: Headers>(cfg: &ProviderConfig, headers: &H) -> Result<(), WebhookError> {
    let secret = cfg
        .webhook_secret
        .as_deref()
        .filter(|s| !s.is_empty())
        .ok_or(WebhookError::NotConfigured)?;
    let presented = headers
        .get_header("authorization")
        .ok_or(WebhookError::BadSignature)?;
    if !const_eq_secret(secret.as_bytes(), presented.as_bytes()) {
        return Err(WebhookError::BadSignature);
    }
    Ok(())
}

/// Verify a RevenueCat webhook and, when it reports a refund, return the
/// reversal of the ORIGINAL purchase.
///
/// RevenueCat signals a store refund as a `CANCELLATION` with
/// `cancel_reason = CUSTOMER_SUPPORT`, carrying the refunded purchase's
/// `transaction_id` and a NEGATIVE price. App Store / Play refunds are always
/// for the whole transaction, so a missing or zero price claws back the whole
/// minted deposit.
pub fn verify_reversal_webhook<H: Headers>(
    cfg: &ProviderConfig,
    headers: &H,
    raw: &[u8],
) -> Result<ReversalEvent, WebhookError> {
    verify_authorization(cfg, headers)?;
    let v: Value =
        serde_json::from_slice(raw).map_err(|e| WebhookError::Malformed(e.to_string()))?;
    let event = &v["event"];
    let event_type = event["type"].as_str().unwrap_or("");
    let is_refund = event_type == "CANCELLATION"
        && event["cancel_reason"].as_str() == Some("CUSTOMER_SUPPORT");
    if !is_refund {
        return Err(WebhookError::Ignored(event_type.to_string()));
    }
    let deposit_ref = event["transaction_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| WebhookError::Ignored("refund without a transaction_id".into()))?
        .to_string();
    // USD `price` (negative on a refund) — the same unit the deposit minted in.
    let amount_micros = json_dollars_to_micros(&event["price"])
        .map(i64::abs)
        .filter(|&m| m > 0);
    Ok(ReversalEvent {
        provider: ProviderId::RevenueCat,
        deposit_ref,
        alt_refs: Vec::new(),
        amount_micros,
        reason: ReversalReason::Refund,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Mode;
    use crate::http::{HttpResponse, TransportError};
    use std::collections::HashMap;

    fn cfg(secret: Option<&str>, key: Option<&str>) -> ProviderConfig {
        ProviderConfig {
            provider: ProviderId::RevenueCat,
            api_key: key.map(Into::into),
            webhook_secret: secret.map(Into::into),
            mode: Mode::Sandbox,
            extra: HashMap::new(),
        }
    }

    fn hdrs(auth: &str) -> HashMap<String, String> {
        [("authorization".to_string(), auth.to_string())]
            .into_iter()
            .collect()
    }

    /// A one-shot fake transport returning a canned status + body for GET/POST.
    struct FakeGet(u16, Vec<u8>);

    #[async_trait]
    impl HttpTransport for FakeGet {
        async fn get(
            &self,
            _url: &str,
            _h: &[(String, String)],
        ) -> Result<HttpResponse, TransportError> {
            Ok(HttpResponse {
                status: self.0,
                body: self.1.clone(),
            })
        }
        async fn post_json(
            &self,
            _url: &str,
            _h: &[(String, String)],
            _b: &[u8],
        ) -> Result<HttpResponse, TransportError> {
            Ok(HttpResponse {
                status: self.0,
                body: self.1.clone(),
            })
        }
    }

    #[test]
    fn valid_signed_webhook_yields_exact_micros() {
        let secret = "rc-secret";
        let body = br#"{"event":{"type":"RENEWAL","id":"evt_42","app_user_id":"acct_1","price":"12.34","price_in_purchased_currency":"409.50","currency":"TRY"}}"#;
        let ev = verify_webhook(&cfg(Some(secret), None), &hdrs(secret), body).expect("verifies");
        assert_eq!(ev.amount_micros, 12_340_000);
        assert_eq!(ev.deposit_ref, "evt_42");
        assert_eq!(ev.account_id, "acct_1");
        // This namespaced ref is exactly what the server feeds to reserve.mint as
        // both deposit_ref and idem_key, so an exact 12.34 USD purchase mints
        // exactly 12_340_000 µAIT (deposit-gated, dedupe by this key).
        assert_eq!(ev.namespaced_ref(), "revenuecat:evt_42");
    }

    #[test]
    fn tampered_signature_is_rejected() {
        let body = br#"{"event":{"type":"INITIAL_PURCHASE","id":"e","app_user_id":"a","price":1.0}}"#;
        assert_eq!(
            verify_webhook(&cfg(Some("real-secret"), None), &hdrs("tampered"), body),
            Err(WebhookError::BadSignature)
        );
    }

    #[test]
    fn unconfigured_secret_is_not_configured() {
        let body = br#"{"event":{"type":"INITIAL_PURCHASE"}}"#;
        assert_eq!(
            verify_webhook(&cfg(None, None), &hdrs("anything"), body),
            Err(WebhookError::NotConfigured)
        );
        // An empty-string secret is treated as unset (BYOK inactive), same as
        // the Stripe empty-secret guard.
        assert_eq!(
            verify_webhook(&cfg(Some(""), None), &hdrs("anything"), body),
            Err(WebhookError::NotConfigured)
        );
    }

    #[test]
    fn unknown_event_type_is_ignored() {
        let secret = "rc-secret";
        let body = br#"{"event":{"type":"CANCELLATION","id":"e","app_user_id":"a","price_in_purchased_currency":1.0}}"#;
        assert_eq!(
            verify_webhook(&cfg(Some(secret), None), &hdrs(secret), body),
            Err(WebhookError::Ignored("CANCELLATION".into()))
        );
    }

    fn subscriber(record: &str) -> Vec<u8> {
        format!(
            r#"{{"subscriber":{{"non_subscriptions":{{"credits_10":[{record}]}},"subscriptions":{{}}}}}}"#
        )
        .into_bytes()
    }

    #[tokio::test]
    async fn confirm_finds_transaction_under_subscriber_and_binds_account() {
        let body = subscriber(r#"{"id":"rc_1","store_transaction_id":"txn_9","refunded_at":null}"#);
        let ramp = RevenueCatRamp::new(Arc::new(FakeGet(200, body)), cfg(Some("s"), Some("k")));
        let dep = ramp
            .confirm_deposit_for("txn_9", "acct_1", Micros::new(7_500_000))
            .await
            .expect("confirmed");
        assert_eq!(dep.amount_micros, Micros::new(7_500_000));
        assert_eq!(dep.bound_account.as_deref(), Some("acct_1"));
        assert!(dep.confirmed);
    }

    #[tokio::test]
    async fn confirm_uses_usd_subscription_price_as_cap() {
        let body = br#"{"subscriber":{"non_subscriptions":{},"subscriptions":{"pro":{"store_transaction_id":"txn_s","price":{"amount":4.99,"currency":"USD"}}}}}"#.to_vec();
        let ramp = RevenueCatRamp::new(Arc::new(FakeGet(200, body)), cfg(Some("s"), Some("k")));
        let dep = ramp
            .confirm_deposit_for("txn_s", "acct_1", Micros::new(9_000_000))
            .await
            .unwrap();
        assert_eq!(dep.amount_micros, Micros::new(4_990_000));
    }

    #[tokio::test]
    async fn unknown_transaction_or_404_is_not_found() {
        let ramp = RevenueCatRamp::new(
            Arc::new(FakeGet(200, subscriber(r#"{"store_transaction_id":"other"}"#))),
            cfg(Some("s"), Some("k")),
        );
        assert_eq!(
            ramp.confirm_deposit_for("txn_9", "acct_1", Micros::new(1)).await,
            Err(RampError::DepositNotFound)
        );
        let ramp = RevenueCatRamp::new(
            Arc::new(FakeGet(404, b"{}".to_vec())),
            cfg(Some("s"), Some("k")),
        );
        assert_eq!(
            ramp.confirm_deposit_for("txn_9", "acct_1", Micros::new(1)).await,
            Err(RampError::DepositNotFound)
        );
    }

    #[tokio::test]
    async fn refunded_transaction_is_not_confirmed() {
        let body = subscriber(r#"{"store_transaction_id":"txn_9","refunded_at":"2026-01-01T00:00:00Z"}"#);
        let ramp = RevenueCatRamp::new(Arc::new(FakeGet(200, body)), cfg(Some("s"), Some("k")));
        assert_eq!(
            ramp.confirm_deposit_for("txn_9", "acct_1", Micros::new(1)).await,
            Err(RampError::NotConfirmed)
        );
    }

    #[tokio::test]
    async fn no_payout_supported() {
        let ramp = RevenueCatRamp::new(
            Arc::new(FakeGet(200, b"{}".to_vec())),
            cfg(Some("s"), Some("k")),
        );
        let dest = PayoutDest::Stripe {
            account: "acct".into(),
        };
        assert_eq!(
            ramp.initiate_payout("revenuecat:1", &dest, Micros::new(1))
                .await,
            Err(RampError::ProviderUnavailable)
        );
    }
}
