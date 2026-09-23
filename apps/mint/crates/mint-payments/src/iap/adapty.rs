//! Adapty adapter (mobile IAP, webhook-only). Webhook authenticity:
//! `Authorization` shared-secret equality (constant time), with optional HMAC
//! support. Events `subscription_started` / `non_subscription_purchase`;
//! `customer_user_id` → `account_id`. No server checkout, no payout.

use std::sync::Arc;

use async_trait::async_trait;
use mint_core::Micros;
use mint_reserve::{Asset, Deposit, Payout, PayoutDest, Ramp, RampError};
use serde_json::Value;

use crate::config::ProviderConfig;
use crate::event::{DepositEvent, ReversalEvent, ReversalReason, WebhookError};
use crate::http::{map_status, Headers, HttpTransport};
use crate::money::json_dollars_to_micros;
use crate::provider::ProviderId;
use crate::verify::const_eq_secret;

const API_BASE: &str = "https://api.adapty.io";

/// A live Adapty [`Ramp`] over an injected [`HttpTransport`].
pub struct AdaptyRamp {
    transport: Arc<dyn HttpTransport>,
    cfg: ProviderConfig,
}

impl AdaptyRamp {
    /// Build the adapter from its transport + resolved config.
    pub fn new(transport: Arc<dyn HttpTransport>, cfg: ProviderConfig) -> Self {
        AdaptyRamp { transport, cfg }
    }

    fn auth_headers(&self) -> Vec<(String, String)> {
        let key = self.cfg.api_key.clone().unwrap_or_default();
        vec![("authorization".into(), format!("Api-Key {key}"))]
    }
}

#[async_trait]
impl Ramp for AdaptyRamp {
    async fn confirm_deposit(&self, deposit_ref: &str) -> Result<Deposit, RampError> {
        let url = format!("{API_BASE}/api/v2/server-side-api/purchase/{deposit_ref}/");
        let resp = map_status(self.transport.get(&url, &self.auth_headers()).await)?;
        let v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        // Fail closed on a refunded/voided purchase: a later refund or chargeback
        // must NOT mint. Adapty marks these with `is_refund`/`refunded_at` or a
        // non-active status.
        if is_refunded(&v) {
            return Err(RampError::NotConfirmed);
        }
        // Fail closed on a missing/zero settled amount rather than minting 0
        // against an unresolved purchase (never `.unwrap_or(0)`).
        let micros = json_dollars_to_micros(&v["price_usd"])
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

/// Whether an Adapty purchase payload indicates the transaction was refunded or
/// voided, in which case it must not mint. Recognizes `is_refund: true`, a
/// non-null `refunded_at`, and an explicit non-active/refunded `status`.
fn is_refunded(v: &Value) -> bool {
    if v["is_refund"].as_bool() == Some(true) || !v["refunded_at"].is_null() {
        return true;
    }
    matches!(
        v["status"].as_str(),
        Some("refunded") | Some("voided") | Some("revoked")
    )
}

/// Verify an Adapty webhook: constant-time `Authorization` equality, then
/// extract the purchase.
pub fn verify_webhook<H: Headers>(
    cfg: &ProviderConfig,
    headers: &H,
    raw: &[u8],
) -> Result<DepositEvent, WebhookError> {
    verify_authorization(cfg, headers)?;

    let v: Value =
        serde_json::from_slice(raw).map_err(|e| WebhookError::Malformed(e.to_string()))?;
    let event_type = v["event_type"].as_str().unwrap_or("");
    if !matches!(
        event_type,
        "subscription_started" | "non_subscription_purchase"
    ) {
        return Err(WebhookError::Ignored(event_type.to_string()));
    }
    let props = &v["event_properties"];
    let deposit_ref = props["transaction_id"]
        .as_str()
        .ok_or_else(|| WebhookError::Malformed("missing transaction_id".into()))?
        .to_string();
    let amount_micros = json_dollars_to_micros(&props["price_usd"])
        .or_else(|| json_dollars_to_micros(&props["value"]))
        .ok_or_else(|| WebhookError::Malformed("missing price_usd".into()))?;
    let account_id = props["customer_user_id"]
        .as_str()
        .ok_or_else(|| WebhookError::Malformed("missing customer_user_id".into()))?
        .to_string();
    Ok(DepositEvent {
        provider: ProviderId::Adapty,
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

/// Verify an Adapty webhook and, when it reports a refund, return the
/// reversal of the ORIGINAL purchase (Mint credits Adapty under the store
/// `transaction_id`, which the refund event repeats).
///
/// Events `subscription_refunded` / `non_subscription_purchase_refunded`.
/// Store refunds are for the whole transaction; the refunded `price_usd` is
/// used when present, otherwise the whole minted deposit is clawed back.
pub fn verify_reversal_webhook<H: Headers>(
    cfg: &ProviderConfig,
    headers: &H,
    raw: &[u8],
) -> Result<ReversalEvent, WebhookError> {
    verify_authorization(cfg, headers)?;
    let v: Value =
        serde_json::from_slice(raw).map_err(|e| WebhookError::Malformed(e.to_string()))?;
    let event_type = v["event_type"].as_str().unwrap_or("");
    if !matches!(
        event_type,
        "subscription_refunded" | "non_subscription_purchase_refunded"
    ) {
        return Err(WebhookError::Ignored(event_type.to_string()));
    }
    let props = &v["event_properties"];
    let deposit_ref = props["transaction_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| WebhookError::Ignored(format!("{event_type} without a transaction_id")))?
        .to_string();
    let amount_micros = json_dollars_to_micros(&props["price_usd"])
        .map(i64::abs)
        .filter(|&m| m > 0);
    Ok(ReversalEvent {
        provider: ProviderId::Adapty,
        deposit_ref,
        // Only the refunded transaction itself: a refunded renewal Mint never
        // credited must not claw back the original purchase.
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
            provider: ProviderId::Adapty,
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
        let secret = "adapty-secret";
        let body = br#"{"event_type":"subscription_started","event_properties":{"transaction_id":"txn_42","customer_user_id":"acct_1","price_usd":"12.34"}}"#;
        let ev = verify_webhook(&cfg(Some(secret), None), &hdrs(secret), body).expect("verifies");
        assert_eq!(ev.amount_micros, 12_340_000);
        assert_eq!(ev.deposit_ref, "txn_42");
        assert_eq!(ev.account_id, "acct_1");
        // Exactly the namespaced ref the server feeds to reserve.mint as
        // deposit_ref + idem_key ⇒ a 12.34 USD purchase mints exactly
        // 12_340_000 µAIT, deduped by this key.
        assert_eq!(ev.namespaced_ref(), "adapty:txn_42");
    }

    #[test]
    fn tampered_signature_is_rejected() {
        let body = br#"{"event_type":"subscription_started","event_properties":{"transaction_id":"t","customer_user_id":"a","price_usd":"1.00"}}"#;
        assert_eq!(
            verify_webhook(&cfg(Some("real-secret"), None), &hdrs("tampered"), body),
            Err(WebhookError::BadSignature)
        );
    }

    #[test]
    fn unconfigured_secret_is_not_configured() {
        let body = br#"{"event_type":"subscription_started"}"#;
        assert_eq!(
            verify_webhook(&cfg(None, None), &hdrs("anything"), body),
            Err(WebhookError::NotConfigured)
        );
        // Empty-string secret ⇒ inactive (BYOK), same as the Stripe guard.
        assert_eq!(
            verify_webhook(&cfg(Some(""), None), &hdrs("anything"), body),
            Err(WebhookError::NotConfigured)
        );
    }

    #[test]
    fn unknown_event_type_is_ignored() {
        let secret = "adapty-secret";
        let body = br#"{"event_type":"subscription_renewed","event_properties":{}}"#;
        assert_eq!(
            verify_webhook(&cfg(Some(secret), None), &hdrs(secret), body),
            Err(WebhookError::Ignored("subscription_renewed".into()))
        );
    }

    #[tokio::test]
    async fn confirm_deposit_reads_exact_price_over_http() {
        let body = br#"{"price_usd":"7.50"}"#.to_vec();
        let ramp = AdaptyRamp::new(Arc::new(FakeGet(200, body)), cfg(Some("s"), Some("k")));
        let dep = ramp.confirm_deposit("txn_1").await.expect("confirmed");
        assert_eq!(dep.amount_micros, Micros::new(7_500_000));
        assert!(dep.confirmed);
        assert_eq!(dep.asset, Asset::Usd);
    }

    #[tokio::test]
    async fn confirm_deposit_maps_404_to_not_found() {
        let ramp = AdaptyRamp::new(
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
        // Settled price present but the purchase was refunded: must not mint.
        let body = br#"{"price_usd":"7.50","is_refund":true}"#.to_vec();
        let ramp = AdaptyRamp::new(Arc::new(FakeGet(200, body)), cfg(Some("s"), Some("k")));
        assert_eq!(
            ramp.confirm_deposit("txn_1").await,
            Err(RampError::NotConfirmed)
        );

        // A refunded status is honored too.
        let body = br#"{"price_usd":"7.50","status":"refunded"}"#.to_vec();
        let ramp = AdaptyRamp::new(Arc::new(FakeGet(200, body)), cfg(Some("s"), Some("k")));
        assert_eq!(
            ramp.confirm_deposit("txn_1").await,
            Err(RampError::NotConfirmed)
        );
    }

    #[tokio::test]
    async fn confirm_deposit_missing_or_zero_price_fails_closed() {
        // Missing price ⇒ NotConfirmed (never mints 0 against an unresolved amount).
        let ramp = AdaptyRamp::new(
            Arc::new(FakeGet(200, b"{}".to_vec())),
            cfg(Some("s"), Some("k")),
        );
        assert_eq!(
            ramp.confirm_deposit("txn_1").await,
            Err(RampError::NotConfirmed)
        );

        // Zero price ⇒ NotConfirmed.
        let body = br#"{"price_usd":"0"}"#.to_vec();
        let ramp = AdaptyRamp::new(Arc::new(FakeGet(200, body)), cfg(Some("s"), Some("k")));
        assert_eq!(
            ramp.confirm_deposit("txn_1").await,
            Err(RampError::NotConfirmed)
        );
    }

    #[tokio::test]
    async fn no_payout_supported() {
        let ramp = AdaptyRamp::new(
            Arc::new(FakeGet(200, b"{}".to_vec())),
            cfg(Some("s"), Some("k")),
        );
        let dest = PayoutDest::Crypto {
            chain: "ethereum".into(),
            address: "0x1".into(),
        };
        assert_eq!(
            ramp.initiate_payout("adapty:1", &dest, Micros::new(1))
                .await,
            Err(RampError::ProviderUnavailable)
        );
    }
}
