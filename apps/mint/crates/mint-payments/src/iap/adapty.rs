//! Adapty adapter (mobile IAP, webhook-only). Webhook authenticity:
//! `Authorization` shared-secret equality (constant time), with optional HMAC
//! support. Events `subscription_started` / `non_subscription_purchase`;
//! `customer_user_id` → `account_id`. No server checkout, no payout.
//!
//! Re-confirmation: Adapty's server-side API is PROFILE-based
//! (`GET /api/v2/server-side-api/profile/` with an `adapty-customer-user-id`
//! header); there is no purchase-by-transaction lookup. The reserve therefore
//! calls [`Ramp::confirm_deposit_for`] with the credited account, and the
//! adapter finds the store transaction inside that profile's
//! `non_subscriptions` / `subscriptions`, fails closed on a refund, and binds
//! the deposit to that account. The profile carries no per-transaction USD
//! amount, so the USD amount is the one from the constant-time-authenticated
//! webhook (`price_usd`).

use std::sync::Arc;

use async_trait::async_trait;
use mint_core::Micros;
use mint_reserve::{Asset, Deposit, Payout, PayoutDest, Ramp, RampError};
use serde_json::Value;

use crate::config::ProviderConfig;
use crate::event::{DepositEvent, ReversalEvent, ReversalReason, WebhookError};
use crate::http::{map_status, Headers, HttpTransport};
use crate::iap::find_transaction;
use crate::money::json_dollars_to_micros;
use crate::provider::ProviderId;
use crate::verify::const_eq_secret;

const API_BASE: &str = "https://api.adapty.io";

/// The record keys Adapty uses for a purchase's store transaction id (v2
/// `store_*`, legacy `vendor_*`, and the original id a `subscription_started`
/// event reports for the first period).
const TXN_KEYS: [&str; 5] = [
    "store_transaction_id",
    "vendor_transaction_id",
    "store_original_transaction_id",
    "vendor_original_transaction_id",
    "transaction_id",
];

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

    /// Fetch the profile for `customer_user_id` and locate `transaction_id`.
    async fn lookup(&self, customer_user_id: &str, transaction_id: &str) -> Result<Value, RampError> {
        if customer_user_id.trim().is_empty() || transaction_id.trim().is_empty() {
            return Err(RampError::DepositNotFound);
        }
        let url = format!("{API_BASE}/api/v2/server-side-api/profile/");
        let mut headers = self.auth_headers();
        headers.push((
            "adapty-customer-user-id".into(),
            customer_user_id.to_string(),
        ));
        let resp = map_status(self.transport.get(&url, &headers).await)?;
        let v: Value =
            serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        let profile = if v["data"].is_object() { &v["data"] } else { &v };
        find_transaction(
            &[&profile["non_subscriptions"], &profile["subscriptions"]],
            &TXN_KEYS,
            transaction_id,
        )
        .cloned()
        .ok_or(RampError::DepositNotFound)
    }
}

#[async_trait]
impl Ramp for AdaptyRamp {
    async fn confirm_deposit(&self, _deposit_ref: &str) -> Result<Deposit, RampError> {
        // Adapty cannot be queried by transaction id alone (profile-keyed API);
        // the reserve always uses `confirm_deposit_for`.
        Err(RampError::DepositNotFound)
    }

    async fn confirm_deposit_for(
        &self,
        deposit_ref: &str,
        account: &str,
        amount: Micros,
    ) -> Result<Deposit, RampError> {
        let record = self.lookup(account, deposit_ref).await?;
        // Fail closed on a refunded/voided purchase: a later refund or chargeback
        // must NOT mint.
        if is_refunded(&record) {
            return Err(RampError::NotConfirmed);
        }
        if amount.get() <= 0 {
            return Err(RampError::NotConfirmed);
        }
        let confirmed = json_dollars_to_micros(&record["price_usd"])
            .filter(|&m| m > 0)
            .unwrap_or(amount.get());
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
    async fn confirm_finds_transaction_in_profile_and_binds_account() {
        let body = br#"{"data":{"customer_user_id":"acct_1","non_subscriptions":[{"purchase_id":"p1","store_transaction_id":"txn_1","is_refund":false}],"subscriptions":[]}}"#.to_vec();
        let ramp = AdaptyRamp::new(Arc::new(FakeGet(200, body)), cfg(Some("s"), Some("k")));
        let dep = ramp
            .confirm_deposit_for("txn_1", "acct_1", Micros::new(7_500_000))
            .await
            .expect("confirmed");
        assert_eq!(dep.amount_micros, Micros::new(7_500_000));
        assert_eq!(dep.bound_account.as_deref(), Some("acct_1"));
    }

    #[tokio::test]
    async fn missing_or_refunded_transaction_does_not_mint() {
        let body = br#"{"data":{"non_subscriptions":[{"store_transaction_id":"other"}]}}"#.to_vec();
        let ramp = AdaptyRamp::new(Arc::new(FakeGet(200, body)), cfg(Some("s"), Some("k")));
        assert_eq!(
            ramp.confirm_deposit_for("txn_1", "acct_1", Micros::new(1)).await,
            Err(RampError::DepositNotFound)
        );
        let body = br#"{"data":{"subscriptions":[{"store_transaction_id":"txn_1","is_refund":true}]}}"#.to_vec();
        let ramp = AdaptyRamp::new(Arc::new(FakeGet(200, body)), cfg(Some("s"), Some("k")));
        assert_eq!(
            ramp.confirm_deposit_for("txn_1", "acct_1", Micros::new(1)).await,
            Err(RampError::NotConfirmed)
        );
        let ramp = AdaptyRamp::new(
            Arc::new(FakeGet(404, b"{}".to_vec())),
            cfg(Some("s"), Some("k")),
        );
        assert_eq!(
            ramp.confirm_deposit_for("txn_1", "acct_1", Micros::new(1)).await,
            Err(RampError::DepositNotFound)
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
