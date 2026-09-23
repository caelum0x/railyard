//! RevenueCat adapter (mobile IAP, webhook-only). Webhook authenticity: the
//! `Authorization` header must EXACTLY equal the configured shared secret,
//! compared in constant time (NOT an HMAC). Events `INITIAL_PURCHASE` /
//! `RENEWAL` / `NON_RENEWING_PURCHASE`; `app_user_id` → `account_id`,
//! `price_in_purchased_currency` → µ$. No server checkout, no payout.

use std::sync::Arc;

use async_trait::async_trait;
use mint_core::Micros;
use mint_reserve::{Asset, Deposit, Payout, PayoutDest, Ramp, RampError};
use serde_json::Value;

use crate::config::ProviderConfig;
use crate::event::{DepositEvent, WebhookError};
use crate::http::{map_status, Headers, HttpTransport};
use crate::money::json_dollars_to_micros;
use crate::provider::ProviderId;
use crate::verify::const_eq_secret;

const API_BASE: &str = "https://api.revenuecat.com";

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
}

#[async_trait]
impl Ramp for RevenueCatRamp {
    async fn confirm_deposit(&self, deposit_ref: &str) -> Result<Deposit, RampError> {
        // Re-confirm the subscriber/transaction against RevenueCat's REST API.
        // The webhook `Authorization` equality is the primary authenticity gate;
        // this GET guards against a spoofed/duplicated delivery.
        let url = format!("{API_BASE}/v1/subscribers/{deposit_ref}");
        let resp = map_status(self.transport.get(&url, &self.auth_headers()).await)?;
        let v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        // Fail closed on a refunded/revoked purchase: a later refund or chargeback
        // must NOT mint. RevenueCat marks these with a non-null `refunded_at` (or
        // an explicit `is_refunded`/revoked entitlement).
        if is_refunded(&v) {
            return Err(RampError::NotConfirmed);
        }
        // Fail closed on a missing/zero settled amount rather than minting 0
        // against an unresolved transaction (never `.unwrap_or(0)`).
        let micros = json_dollars_to_micros(&v["price_in_purchased_currency"])
            .filter(|&m| m > 0)
            .ok_or(RampError::NotConfirmed)?;
        Ok(Deposit {
            deposit_ref: deposit_ref.to_string(),
            amount_micros: Micros::new(micros),
            asset: Asset::Usd,
            confirmed: true,
            bound_account: None,
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
    let deposit_ref = event["id"]
        .as_str()
        .or_else(|| event["transaction_id"].as_str())
        .ok_or_else(|| WebhookError::Malformed("missing event id".into()))?
        .to_string();
    let amount_micros = json_dollars_to_micros(&event["price_in_purchased_currency"])
        .ok_or_else(|| WebhookError::Malformed("missing price".into()))?;
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
        let body = br#"{"event":{"type":"RENEWAL","id":"evt_42","app_user_id":"acct_1","price_in_purchased_currency":"12.34"}}"#;
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
        let body = br#"{"event":{"type":"INITIAL_PURCHASE","id":"e","app_user_id":"a","price_in_purchased_currency":1.0}}"#;
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

    #[tokio::test]
    async fn confirm_deposit_reads_exact_price_over_http() {
        let body = br#"{"price_in_purchased_currency":"7.50"}"#.to_vec();
        let ramp = RevenueCatRamp::new(Arc::new(FakeGet(200, body)), cfg(Some("s"), Some("k")));
        let dep = ramp.confirm_deposit("sub_1").await.expect("confirmed");
        assert_eq!(dep.amount_micros, Micros::new(7_500_000));
        assert!(dep.confirmed);
        assert_eq!(dep.asset, Asset::Usd);
    }

    #[tokio::test]
    async fn confirm_deposit_maps_404_to_not_found() {
        let ramp = RevenueCatRamp::new(
            Arc::new(FakeGet(404, b"{}".to_vec())),
            cfg(Some("s"), Some("k")),
        );
        assert_eq!(
            ramp.confirm_deposit("missing").await,
            Err(RampError::DepositNotFound)
        );
    }

    #[tokio::test]
    async fn confirm_deposit_refunded_purchase_is_not_confirmed() {
        // A settled price is present but the transaction was refunded: must not mint.
        let body =
            br#"{"price_in_purchased_currency":"7.50","refunded_at":"2026-01-01T00:00:00Z"}"#
                .to_vec();
        let ramp = RevenueCatRamp::new(Arc::new(FakeGet(200, body)), cfg(Some("s"), Some("k")));
        assert_eq!(
            ramp.confirm_deposit("sub_1").await,
            Err(RampError::NotConfirmed)
        );

        // Explicit is_refunded flag is honored too.
        let body = br#"{"price_in_purchased_currency":"7.50","is_refunded":true}"#.to_vec();
        let ramp = RevenueCatRamp::new(Arc::new(FakeGet(200, body)), cfg(Some("s"), Some("k")));
        assert_eq!(
            ramp.confirm_deposit("sub_1").await,
            Err(RampError::NotConfirmed)
        );
    }

    #[tokio::test]
    async fn confirm_deposit_missing_or_zero_price_fails_closed() {
        // Missing price ⇒ NotConfirmed (never mints 0 against an unresolved amount).
        let ramp = RevenueCatRamp::new(
            Arc::new(FakeGet(200, b"{}".to_vec())),
            cfg(Some("s"), Some("k")),
        );
        assert_eq!(
            ramp.confirm_deposit("sub_1").await,
            Err(RampError::NotConfirmed)
        );

        // Zero price ⇒ NotConfirmed.
        let body = br#"{"price_in_purchased_currency":"0.00"}"#.to_vec();
        let ramp = RevenueCatRamp::new(Arc::new(FakeGet(200, body)), cfg(Some("s"), Some("k")));
        assert_eq!(
            ramp.confirm_deposit("sub_1").await,
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
