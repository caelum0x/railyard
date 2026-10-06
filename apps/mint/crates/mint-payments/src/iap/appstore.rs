//! Apple App Store, direct (no aggregator): App Store Server Notifications v2
//! in, the App Store Server API for re-confirmation.
//!
//! * Webhook: `{ "signedPayload": <JWS> }`. The notification and the
//!   transaction inside it are Apple JWS verified against the pinned Apple
//!   root ([`super::apple_jws`]); the bundle id must be ours and the
//!   environment must match the mode. `ONE_TIME_CHARGE`, `SUBSCRIBED` and
//!   `DID_RENEW` mint; `REFUND` and `REVOKE` reverse. The Mint account is the
//!   transaction's `appAccountToken` (the app sets it to the account id at
//!   purchase). Only USD prices mint (`price` is in milliunits); other
//!   storefront currencies are acknowledged and ignored, since Apple reports
//!   no USD equivalent.
//! * Re-confirmation: `GET /inApps/v1/transactions/{id}` with an ES256 JWT
//!   from the app's App Store Connect API key; the returned transaction is
//!   verified the same way and must not be revoked.
//!
//! Config: `APPSTORE_BUNDLE_ID` (also what marks the webhook as configured),
//! `APPSTORE_KEY_ID`, `APPSTORE_ISSUER_ID`, `APPSTORE_PRIVATE_KEY` (the .p8
//! PEM), optional `APPSTORE_ROOT_SHA256` (default: Apple Root CA - G3).

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use mint_core::Micros;
use mint_reserve::{Asset, Deposit, Payout, PayoutDest, Ramp, RampError};
use ring::rand::SystemRandom;
use ring::signature::{EcdsaKeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
use serde_json::{json, Value};

use super::apple_jws::{AppleJwsVerifier, APPLE_ROOT_CA_G3_SHA256};
use crate::config::{Mode, ProviderConfig};
use crate::event::{DepositEvent, ReversalEvent, ReversalReason, WebhookError};
use crate::http::{encode_component, map_status, Headers, HttpTransport};
use crate::provider::ProviderId;

const LIVE_API: &str = "https://api.storekit.itunes.apple.com";
const SANDBOX_API: &str = "https://api.storekit-sandbox.itunes.apple.com";
const B64URL: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn verifier(cfg: &ProviderConfig) -> AppleJwsVerifier {
    AppleJwsVerifier::new(cfg.extra("root_sha256").filter(|s| !s.is_empty()).unwrap_or(APPLE_ROOT_CA_G3_SHA256))
}

fn expected_env(cfg: &ProviderConfig) -> &'static str {
    match cfg.mode {
        Mode::Live => "Production",
        Mode::Sandbox => "Sandbox",
    }
}

/// A verified notification: its type and the verified transaction.
struct Notification {
    kind: String,
    txn: Value,
}

fn verify_notification(cfg: &ProviderConfig, raw: &[u8], at: i64) -> Result<Notification, WebhookError> {
    let bundle = cfg.webhook_secret.as_deref().filter(|s| !s.is_empty()).ok_or(WebhookError::NotConfigured)?;
    let body: Value = serde_json::from_slice(raw).map_err(|e| WebhookError::Malformed(e.to_string()))?;
    let signed = body["signedPayload"].as_str().ok_or_else(|| WebhookError::Malformed("missing signedPayload".into()))?;
    let v = verifier(cfg);
    let payload = v.verify(signed, at).map_err(|_| WebhookError::BadSignature)?;
    let data = &payload["data"];
    if data["bundleId"].as_str() != Some(bundle) {
        return Err(WebhookError::BadSignature);
    }
    if data["environment"].as_str() != Some(expected_env(cfg)) {
        return Err(WebhookError::Ignored(format!("{} notification on a {} Mint", data["environment"].as_str().unwrap_or("unknown"), expected_env(cfg))));
    }
    let kind = payload["notificationType"].as_str().unwrap_or_default().to_string();
    let txn = match data["signedTransactionInfo"].as_str() {
        Some(jws) => v.verify(jws, at).map_err(|_| WebhookError::BadSignature)?,
        None => return Err(WebhookError::Ignored(format!("{kind} without a transaction"))),
    };
    if txn["bundleId"].as_str() != Some(bundle) {
        return Err(WebhookError::BadSignature);
    }
    Ok(Notification { kind, txn })
}

/// USD price of a transaction in µ$ (`price` is in milliunits).
fn usd_micros(txn: &Value) -> Option<i64> {
    if !txn["currency"].as_str().is_some_and(|c| c.eq_ignore_ascii_case("USD")) {
        return None;
    }
    txn["price"].as_i64().filter(|p| *p > 0).and_then(|p| p.checked_mul(1000))
}

fn txn_id(txn: &Value) -> Result<String, WebhookError> {
    txn["transactionId"].as_str().filter(|s| !s.is_empty()).map(str::to_string).ok_or_else(|| WebhookError::Malformed("missing transactionId".into()))
}

/// Verify a notification that mints.
pub fn verify_webhook<H: Headers>(cfg: &ProviderConfig, _headers: &H, raw: &[u8]) -> Result<DepositEvent, WebhookError> {
    verify_webhook_at(cfg, raw, now())
}

pub(crate) fn verify_webhook_at(cfg: &ProviderConfig, raw: &[u8], at: i64) -> Result<DepositEvent, WebhookError> {
    let n = verify_notification(cfg, raw, at)?;
    if !matches!(n.kind.as_str(), "ONE_TIME_CHARGE" | "SUBSCRIBED" | "DID_RENEW") {
        return Err(WebhookError::Ignored(n.kind));
    }
    if !n.txn["revocationDate"].is_null() {
        return Err(WebhookError::Ignored("revoked transaction".into()));
    }
    let amount_micros = usd_micros(&n.txn).ok_or_else(|| WebhookError::Ignored("non-USD storefront price".into()))?;
    let account_id = n.txn["appAccountToken"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| WebhookError::Malformed("missing appAccountToken (set it to the Mint account at purchase)".into()))?.to_string();
    Ok(DepositEvent { provider: ProviderId::AppStore, deposit_ref: txn_id(&n.txn)?, amount_micros, asset: Asset::Usd, account_id })
}

/// Verify a `REFUND` / `REVOKE` notification as the reversal of its purchase.
pub fn verify_reversal_webhook<H: Headers>(cfg: &ProviderConfig, _headers: &H, raw: &[u8]) -> Result<ReversalEvent, WebhookError> {
    verify_reversal_at(cfg, raw, now())
}

pub(crate) fn verify_reversal_at(cfg: &ProviderConfig, raw: &[u8], at: i64) -> Result<ReversalEvent, WebhookError> {
    let n = verify_notification(cfg, raw, at)?;
    if !matches!(n.kind.as_str(), "REFUND" | "REVOKE") {
        return Err(WebhookError::Ignored(n.kind));
    }
    let original = n.txn["originalTransactionId"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
    let id = txn_id(&n.txn)?;
    Ok(ReversalEvent {
        provider: ProviderId::AppStore,
        alt_refs: original.into_iter().filter(|o| *o != id).collect(),
        deposit_ref: id,
        // App Store refunds are for the whole transaction.
        amount_micros: None,
        reason: ReversalReason::Refund,
    })
}

/// The App Store Server API client.
pub struct AppStoreRamp {
    transport: Arc<dyn HttpTransport>,
    cfg: ProviderConfig,
}

impl AppStoreRamp {
    pub fn new(transport: Arc<dyn HttpTransport>, cfg: ProviderConfig) -> Self {
        AppStoreRamp { transport, cfg }
    }

    /// The App Store Server API bearer token (ES256, 20 minutes).
    pub fn api_token(&self, at: i64) -> Result<String, RampError> {
        let pem = self.cfg.api_key.as_deref().ok_or(RampError::ProviderUnavailable)?;
        let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
        let der = base64::engine::general_purpose::STANDARD.decode(b64.trim()).map_err(|_| RampError::ProviderUnavailable)?;
        let rng = SystemRandom::new();
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &der, &rng).map_err(|_| RampError::ProviderUnavailable)?;
        let kid = self.cfg.extra("key_id").ok_or(RampError::ProviderUnavailable)?;
        let iss = self.cfg.extra("issuer_id").ok_or(RampError::ProviderUnavailable)?;
        let bid = self.cfg.webhook_secret.as_deref().ok_or(RampError::ProviderUnavailable)?;
        let header = json!({ "alg": "ES256", "kid": kid, "typ": "JWT" });
        let claims = json!({ "iss": iss, "iat": at, "exp": at + 1200, "aud": "appstoreconnect-v1", "bid": bid });
        let input = format!("{}.{}", B64URL.encode(header.to_string()), B64URL.encode(claims.to_string()));
        let sig = key.sign(&rng, input.as_bytes()).map_err(|_| RampError::ProviderUnavailable)?;
        Ok(format!("{input}.{}", B64URL.encode(sig.as_ref())))
    }

    async fn lookup(&self, transaction_id: &str) -> Result<Value, RampError> {
        if transaction_id.trim().is_empty() {
            return Err(RampError::DepositNotFound);
        }
        let base = match self.cfg.mode {
            Mode::Live => LIVE_API,
            Mode::Sandbox => SANDBOX_API,
        };
        let url = format!("{base}/inApps/v1/transactions/{}", encode_component(transaction_id));
        let auth = vec![("authorization".to_string(), format!("Bearer {}", self.api_token(now())?))];
        let resp = map_status(self.transport.get(&url, &auth).await)?;
        let v: Value = serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        let jws = v["signedTransactionInfo"].as_str().ok_or(RampError::DepositNotFound)?;
        let txn = verifier(&self.cfg).verify(jws, now()).map_err(|_| RampError::NotConfirmed)?;
        if txn["bundleId"].as_str() != self.cfg.webhook_secret.as_deref() || txn["transactionId"].as_str() != Some(transaction_id) {
            return Err(RampError::NotConfirmed);
        }
        Ok(txn)
    }

    fn deposit(txn: &Value, deposit_ref: &str) -> Result<Deposit, RampError> {
        if !txn["revocationDate"].is_null() {
            return Err(RampError::NotConfirmed);
        }
        let amount = usd_micros(txn).ok_or(RampError::NotConfirmed)?;
        Ok(Deposit {
            deposit_ref: deposit_ref.to_string(),
            amount_micros: Micros::new(amount),
            asset: Asset::Usd,
            confirmed: true,
            bound_account: txn["appAccountToken"].as_str().map(str::to_string),
        })
    }
}

#[async_trait]
impl Ramp for AppStoreRamp {
    async fn confirm_deposit(&self, deposit_ref: &str) -> Result<Deposit, RampError> {
        let txn = self.lookup(deposit_ref).await?;
        Self::deposit(&txn, deposit_ref)
    }

    async fn confirm_deposit_for(&self, deposit_ref: &str, account: &str, amount: Micros) -> Result<Deposit, RampError> {
        let txn = self.lookup(deposit_ref).await?;
        let d = Self::deposit(&txn, deposit_ref)?;
        if d.bound_account.as_deref() != Some(account) || amount.get() > d.amount_micros.get() {
            return Err(RampError::NotConfirmed);
        }
        Ok(d)
    }

    async fn initiate_payout(&self, _payout_ref: &str, _dest: &PayoutDest, _amount: Micros) -> Result<Payout, RampError> {
        Err(RampError::ProviderUnavailable)
    }

    async fn check_payout(&self, _payout_ref: &str, _dest: &PayoutDest, _amount: Micros) -> Result<(), RampError> {
        Err(RampError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::{HttpResponse, TransportError};
    use crate::iap::apple_jws::testing::Chain;
    use std::collections::HashMap;
    use std::sync::Mutex;

    fn cfg(chain: &Chain, mode: Mode) -> ProviderConfig {
        let rng = SystemRandom::new();
        let p8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).expect("p8");
        let pem = format!("-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n", base64::engine::general_purpose::STANDARD.encode(p8.as_ref()));
        ProviderConfig {
            provider: ProviderId::AppStore,
            api_key: Some(pem),
            webhook_secret: Some("com.acme.app".into()),
            mode,
            extra: HashMap::from([("root_sha256".to_string(), chain.root_sha256.clone()), ("key_id".to_string(), "KEY123".to_string()), ("issuer_id".to_string(), "iss-uuid".to_string())]),
        }
    }

    fn txn(chain: &Chain, extra: Value) -> String {
        let mut t = json!({ "transactionId": "2000000111", "originalTransactionId": "2000000100", "bundleId": "com.acme.app", "productId": "credits_10",
            "appAccountToken": "acct_ada", "price": 9990, "currency": "USD", "environment": "Sandbox" });
        if let (Some(t), Some(e)) = (t.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                t.insert(k.clone(), v.clone());
            }
        }
        chain.sign(&t)
    }

    fn notification(chain: &Chain, kind: &str, signed_txn: String, env: &str) -> Vec<u8> {
        let payload = json!({ "notificationType": kind, "notificationUUID": "u", "data": { "bundleId": "com.acme.app", "environment": env, "signedTransactionInfo": signed_txn } });
        json!({ "signedPayload": chain.sign(&payload) }).to_string().into_bytes()
    }

    #[test]
    fn a_verified_purchase_mints_usd_for_the_app_account() {
        let chain = Chain::new(true);
        let c = cfg(&chain, Mode::Sandbox);
        let ev = verify_webhook_at(&c, &notification(&chain, "ONE_TIME_CHARGE", txn(&chain, json!({})), "Sandbox"), now()).unwrap();
        assert_eq!((ev.deposit_ref.as_str(), ev.amount_micros, ev.account_id.as_str()), ("2000000111", 9_990_000, "acct_ada"));
        assert_eq!(ev.provider, ProviderId::AppStore);
    }

    #[test]
    fn foreign_bundles_environments_currencies_and_forgeries_do_not_mint() {
        let chain = Chain::new(true);
        let c = cfg(&chain, Mode::Sandbox);
        let other_bundle = txn(&chain, json!({ "bundleId": "com.evil.app" }));
        assert!(matches!(verify_webhook_at(&c, &notification(&chain, "ONE_TIME_CHARGE", other_bundle, "Sandbox"), now()), Err(WebhookError::BadSignature)));
        assert!(matches!(verify_webhook_at(&c, &notification(&chain, "ONE_TIME_CHARGE", txn(&chain, json!({})), "Production"), now()), Err(WebhookError::Ignored(_))));
        let eur = txn(&chain, json!({ "currency": "EUR" }));
        assert!(matches!(verify_webhook_at(&c, &notification(&chain, "ONE_TIME_CHARGE", eur, "Sandbox"), now()), Err(WebhookError::Ignored(_))));
        // Signed by another chain than the pinned root.
        let rogue = Chain::new(true);
        assert!(matches!(verify_webhook_at(&c, &notification(&rogue, "ONE_TIME_CHARGE", txn(&rogue, json!({})), "Sandbox"), now()), Err(WebhookError::BadSignature)));
        assert!(matches!(verify_webhook_at(&c, &notification(&chain, "DID_CHANGE_RENEWAL_STATUS", txn(&chain, json!({})), "Sandbox"), now()), Err(WebhookError::Ignored(_))));
    }

    #[test]
    fn refunds_reverse_the_original_purchase() {
        let chain = Chain::new(true);
        let c = cfg(&chain, Mode::Sandbox);
        let rev = verify_reversal_at(&c, &notification(&chain, "REFUND", txn(&chain, json!({ "revocationDate": 1 })), "Sandbox"), now()).unwrap();
        assert_eq!((rev.deposit_ref.as_str(), rev.alt_refs.clone(), rev.amount_micros), ("2000000111", vec!["2000000100".to_string()], None));
    }

    struct Fake {
        body: Vec<u8>,
        seen: Mutex<Vec<(String, String)>>,
    }

    #[async_trait]
    impl HttpTransport for Fake {
        async fn get(&self, url: &str, h: &[(String, String)]) -> Result<HttpResponse, TransportError> {
            self.seen.lock().expect("lock").push((url.to_string(), h.first().map(|x| x.1.clone()).unwrap_or_default()));
            Ok(HttpResponse { status: 200, body: self.body.clone() })
        }
        async fn post_json(&self, _u: &str, _h: &[(String, String)], _b: &[u8]) -> Result<HttpResponse, TransportError> {
            Err(TransportError("unused".into()))
        }
    }

    #[tokio::test]
    async fn the_server_api_reconfirms_with_a_signed_token() {
        let chain = Chain::new(true);
        let c = cfg(&chain, Mode::Sandbox);
        let fake = Arc::new(Fake { body: json!({ "signedTransactionInfo": txn(&chain, json!({})) }).to_string().into_bytes(), seen: Mutex::new(Vec::new()) });
        let ramp = AppStoreRamp::new(fake.clone(), c.clone());
        let d = ramp.confirm_deposit_for("2000000111", "acct_ada", Micros::new(9_990_000)).await.unwrap();
        assert_eq!((d.amount_micros.get(), d.bound_account.as_deref()), (9_990_000, Some("acct_ada")));
        assert!(matches!(ramp.confirm_deposit_for("2000000111", "acct_eve", Micros::new(1)).await, Err(RampError::NotConfirmed)));
        let (url, auth) = fake.seen.lock().expect("lock")[0].clone();
        assert_eq!(url, "https://api.storekit-sandbox.itunes.apple.com/inApps/v1/transactions/2000000111");
        let claims: Value = serde_json::from_slice(&B64URL.decode(auth.trim_start_matches("Bearer ").split('.').nth(1).unwrap()).unwrap()).unwrap();
        assert_eq!((claims["aud"].as_str(), claims["bid"].as_str(), claims["iss"].as_str()), (Some("appstoreconnect-v1"), Some("com.acme.app"), Some("iss-uuid")));
        // A revoked transaction never confirms.
        let revoked = AppStoreRamp::new(Arc::new(Fake { body: json!({ "signedTransactionInfo": txn(&chain, json!({ "revocationDate": 5 })) }).to_string().into_bytes(), seen: Mutex::new(Vec::new()) }), c);
        assert!(matches!(revoked.confirm_deposit("2000000111").await, Err(RampError::NotConfirmed)));
    }
}
