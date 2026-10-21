//! Google Play, direct (no aggregator): Real-time Developer Notifications
//! (RTDN) in, the Play Developer API for re-confirmation.
//!
//! * Webhook: Google Cloud Pub/Sub push envelope
//!   `{ "message": { "data": <base64 DeveloperNotification JSON> } }`.
//!   Unlike Apple's JWS-signed payload, an RTDN message carries no
//!   self-authenticating signature `mint-payments` can check offline, so
//!   authenticity is a shared token compared in CONSTANT TIME (same family as
//!   [`crate::verify::const_eq_secret`], not Apple's certificate chain). The
//!   operator puts the token in the Pub/Sub push subscription's endpoint URL
//!   (`.../v1/deposits/playstore/webhook?token=...`); `mint-server` bridges
//!   that query parameter into the `x-playstore-token` header before calling
//!   here, since this crate stays axum/query-string free (see the module doc
//!   on [`crate`]).
//! * An RTDN notification carries neither a price nor the Mint account to
//!   credit (unlike Apple's signed transaction or RevenueCat's subscriber
//!   record), so a mintable notification (`subscriptionNotification` with
//!   `PURCHASED`/`RENEWED`/`RECOVERED`/`RESTARTED`, or
//!   `oneTimeProductNotification` with `PURCHASED`) is re-confirmed against
//!   the Play Developer API (OAuth2 service-account bearer) BEFORE it is
//!   minted: that call is the only place the account id
//!   (`obfuscatedExternalAccountId`, which the app must set to the Mint
//!   account at purchase, mirroring Apple's `appAccountToken`) and the order
//!   id (which, unlike the purchase token, is unique PER CHARGE — Google
//!   suffixes it `..0`, `..1`, `..2` for each renewal of the same
//!   subscription) are available. The USD price has no API field either (Play
//!   purchase resources never carry one): it comes from a static catalog,
//!   `PLAYSTORE_PRICES_JSON` (USD cents per product/subscription id), the same
//!   fixed-price-catalog pattern [`crate::card::creem`] / [`crate::card::dodo`]
//!   already use for their checkout products.
//! * `voidedPurchaseNotification` (Google's refund/chargeback signal) DOES
//!   carry an `orderId` directly, so [`verify_reversal_webhook`] claws back
//!   without a second API call. `REVOKED` on `subscriptionNotification` /
//!   `CANCELED` on `oneTimeProductNotification` are deliberately `Ignored`
//!   here, not treated as reversals: Google sends `voidedPurchaseNotification`
//!   for the same real-world refund, and that is the one with a reliable
//!   `orderId` to key the clawback on.
//!
//! Config: `PLAYSTORE_SERVICE_ACCOUNT_KEY` (the service account's PKCS8 RSA
//! private key, PEM), `PLAYSTORE_CLIENT_EMAIL` (its `iss`), `PLAYSTORE_PACKAGE_NAME`
//! (the Android application id), `PLAYSTORE_WEBHOOK_TOKEN` (push auth token),
//! `PLAYSTORE_PRICES_JSON` (also what marks the webhook as configured together
//! with the token).

use std::sync::Arc;

use async_trait::async_trait;
use base64::Engine;
use mint_core::Micros;
use mint_reserve::{Asset, Deposit, Payout, PayoutDest, Ramp, RampError};
use ring::rand::SystemRandom;
use ring::signature::{RsaKeyPair, RSA_PKCS1_SHA256};
use serde_json::{json, Value};

use crate::config::ProviderConfig;
use crate::event::{DepositEvent, ReversalEvent, ReversalReason, WebhookError};
use crate::http::{encode_component, map_status, Headers, HttpTransport};
use crate::provider::ProviderId;
use crate::verify::const_eq_secret;

const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const ANDROID_PUBLISHER_SCOPE: &str = "https://www.googleapis.com/auth/androidpublisher";
const API_BASE: &str = "https://androidpublisher.googleapis.com/androidpublisher/v3";
const B64URL: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;
const B64STD: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

/// What kind of Play product a composite `deposit_ref` resolves against — the
/// two Developer API endpoints (subscriptions vs one-time products) differ, so
/// the ref must carry enough to pick one without a second round trip.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Subscription,
    Product,
}

impl Kind {
    fn tag(self) -> &'static str {
        match self {
            Kind::Subscription => "sub",
            Kind::Product => "prod",
        }
    }

    fn from_tag(s: &str) -> Option<Kind> {
        match s {
            "sub" => Some(Kind::Subscription),
            "prod" => Some(Kind::Product),
            _ => None,
        }
    }
}

/// A verified, looked-up Play purchase.
struct PurchaseInfo {
    /// The CURRENT order id for this purchase (`latestOrderId` for a
    /// subscription, `orderId` for a one-time product) — unique per charge.
    order_id: String,
    /// `obfuscatedExternalAccountId`, required: the account binding. A
    /// purchase without one cannot be bound to a Mint account and is refused
    /// (same fail-closed stance as Apple's missing `appAccountToken`).
    account_id: Option<String>,
    /// `true` iff the purchase is in a paid, non-revoked/non-pending state.
    paid: bool,
}

/// Build the composite `deposit_ref`: `"<kind>|<sku>|<token>|<order_id>"`. The
/// order id (not the purchase token, which Play reuses across a
/// subscription's renewals) is what makes this unique per charge.
fn composite_ref(kind: Kind, sku: &str, token: &str, order_id: &str) -> String {
    format!("{}|{}|{}|{}", kind.tag(), sku, token, order_id)
}

struct ParsedRef {
    kind: Kind,
    sku: String,
    token: String,
    order_id: String,
}

fn parse_ref(deposit_ref: &str) -> Option<ParsedRef> {
    let mut parts = deposit_ref.splitn(4, '|');
    let kind = Kind::from_tag(parts.next()?)?;
    let sku = parts.next()?.to_string();
    let token = parts.next()?.to_string();
    let order_id = parts.next()?.to_string();
    if sku.is_empty() || token.is_empty() || order_id.is_empty() {
        return None;
    }
    Some(ParsedRef { kind, sku, token, order_id })
}

/// Resolve `sku`'s USD price (in microdollars) from the `prices_json` extra —
/// a flat `{ "<sku>": <usd_cents> }` map, the same fixed-price-catalog shape
/// [`crate::card::creem`]'s `unit_price_cents` uses.
fn catalog_price_micros(cfg: &ProviderConfig, sku: &str) -> Option<i64> {
    let raw = cfg.extra("prices_json")?;
    let table: Value = serde_json::from_str(raw).ok()?;
    let cents = table.get(sku)?.as_i64()?;
    crate::money::cents_to_micros(cents).filter(|m| *m > 0)
}

/// A live Google Play [`Ramp`] over an injected [`HttpTransport`].
pub struct PlayStoreRamp {
    transport: Arc<dyn HttpTransport>,
    cfg: ProviderConfig,
}

impl PlayStoreRamp {
    /// Build the adapter from its transport + resolved config.
    pub fn new(transport: Arc<dyn HttpTransport>, cfg: ProviderConfig) -> Self {
        PlayStoreRamp { transport, cfg }
    }

    /// Sign a short-lived (1h) RS256 JWT self-assertion and exchange it for a
    /// Developer API OAuth2 access token (the service-account "JWT Bearer
    /// Token" flow — no refresh token, no browser consent).
    async fn access_token(&self) -> Result<String, RampError> {
        let pem = self.cfg.api_key.as_deref().ok_or(RampError::ProviderUnavailable)?;
        let iss = self.cfg.extra("client_email").ok_or(RampError::ProviderUnavailable)?;
        let der = decode_pkcs8_pem(pem).ok_or(RampError::ProviderUnavailable)?;
        let rng = SystemRandom::new();
        let key = RsaKeyPair::from_pkcs8(&der).map_err(|_| RampError::ProviderUnavailable)?;
        let at = now();
        let header = json!({ "alg": "RS256", "typ": "JWT" });
        let claims = json!({
            "iss": iss,
            "scope": ANDROID_PUBLISHER_SCOPE,
            "aud": TOKEN_ENDPOINT,
            "iat": at,
            "exp": at + 3600,
        });
        let input = format!("{}.{}", B64URL.encode(header.to_string()), B64URL.encode(claims.to_string()));
        let mut sig = vec![0u8; key.public().modulus_len()];
        key.sign(&RSA_PKCS1_SHA256, &rng, input.as_bytes(), &mut sig)
            .map_err(|_| RampError::ProviderUnavailable)?;
        let jwt = format!("{input}.{}", B64URL.encode(&sig));

        let body = format!(
            "grant_type={}&assertion={}",
            encode_component("urn:ietf:params:oauth:grant-type:jwt-bearer"),
            encode_component(&jwt)
        );
        let headers = vec![("content-type".to_string(), "application/x-www-form-urlencoded".to_string())];
        let resp = map_status(self.transport.post_json(TOKEN_ENDPOINT, &headers, body.as_bytes()).await)?;
        let v: Value = serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        v["access_token"].as_str().map(str::to_string).ok_or(RampError::ProviderUnavailable)
    }

    /// Look up a purchase token's current state via the Developer API.
    async fn lookup(&self, kind: Kind, sku: &str, token: &str) -> Result<PurchaseInfo, RampError> {
        if sku.trim().is_empty() || token.trim().is_empty() {
            return Err(RampError::DepositNotFound);
        }
        let package = self.cfg.extra("package_name").ok_or(RampError::ProviderUnavailable)?;
        let bearer = self.access_token().await?;
        let auth = vec![("authorization".to_string(), format!("Bearer {bearer}"))];
        let url = match kind {
            Kind::Subscription => format!(
                "{API_BASE}/applications/{}/purchases/subscriptionsv2/tokens/{}",
                encode_component(package),
                encode_component(token)
            ),
            Kind::Product => format!(
                "{API_BASE}/applications/{}/purchases/products/{}/tokens/{}",
                encode_component(package),
                encode_component(sku),
                encode_component(token)
            ),
        };
        let resp = map_status(self.transport.get(&url, &auth).await)?;
        let v: Value = serde_json::from_slice(&resp.body).map_err(|_| RampError::ProviderUnavailable)?;
        Ok(match kind {
            Kind::Subscription => {
                let state = v["subscriptionState"].as_str().unwrap_or("");
                PurchaseInfo {
                    order_id: v["latestOrderId"].as_str().unwrap_or_default().to_string(),
                    account_id: v["externalAccountIdentifiers"]["obfuscatedExternalAccountId"]
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .map(str::to_string),
                    paid: matches!(
                        state,
                        "SUBSCRIPTION_STATE_ACTIVE" | "SUBSCRIPTION_STATE_IN_GRACE_PERIOD" | "SUBSCRIPTION_STATE_CANCELED"
                    ),
                }
            }
            Kind::Product => {
                let state = v["purchaseState"].as_i64().unwrap_or(1);
                PurchaseInfo {
                    order_id: v["orderId"].as_str().unwrap_or_default().to_string(),
                    account_id: v["obfuscatedExternalAccountId"].as_str().filter(|s| !s.is_empty()).map(str::to_string),
                    // 0 == "purchased" (see ProductPurchase#purchaseState).
                    paid: state == 0,
                }
            }
        })
    }
}

#[async_trait]
impl Ramp for PlayStoreRamp {
    async fn confirm_deposit(&self, _deposit_ref: &str) -> Result<Deposit, RampError> {
        // Play has no "look up a purchase by order id alone" endpoint; every
        // lookup needs the purchase token embedded in the composite ref, so
        // the reserve always uses `confirm_deposit_for` (same stance as
        // RevenueCat/Adapty).
        Err(RampError::DepositNotFound)
    }

    async fn confirm_deposit_for(&self, deposit_ref: &str, account: &str, amount: Micros) -> Result<Deposit, RampError> {
        let parsed = parse_ref(deposit_ref).ok_or(RampError::DepositNotFound)?;
        let info = self.lookup(parsed.kind, &parsed.sku, &parsed.token).await?;
        if !info.paid || info.order_id != parsed.order_id {
            return Err(RampError::NotConfirmed);
        }
        let bound = info.account_id.ok_or(RampError::NotConfirmed)?;
        if bound != account {
            return Err(RampError::NotConfirmed);
        }
        let price = catalog_price_micros(&self.cfg, &parsed.sku).ok_or(RampError::ProviderUnavailable)?;
        if amount.get() > price {
            return Err(RampError::NotConfirmed);
        }
        Ok(Deposit {
            deposit_ref: deposit_ref.to_string(),
            amount_micros: Micros::new(price),
            asset: Asset::Usd,
            confirmed: true,
            bound_account: Some(bound),
        })
    }

    async fn initiate_payout(&self, _payout_ref: &str, _dest: &PayoutDest, _amount: Micros) -> Result<Payout, RampError> {
        Err(RampError::ProviderUnavailable)
    }

    async fn check_payout(&self, _payout_ref: &str, _dest: &PayoutDest, _amount: Micros) -> Result<(), RampError> {
        Err(RampError::Unsupported)
    }
}

/// Verify the shared Pub/Sub push token (constant-time) over the
/// `x-playstore-token` header `mint-server` bridges in from the push
/// subscription's `?token=` query parameter.
fn verify_token<H: Headers>(cfg: &ProviderConfig, headers: &H) -> Result<(), WebhookError> {
    let secret = cfg.webhook_secret.as_deref().filter(|s| !s.is_empty()).ok_or(WebhookError::NotConfigured)?;
    let presented = headers.get_header("x-playstore-token").ok_or(WebhookError::BadSignature)?;
    if !const_eq_secret(secret.as_bytes(), presented.as_bytes()) {
        return Err(WebhookError::BadSignature);
    }
    Ok(())
}

/// Decode the Pub/Sub push envelope's `message.data` into the Developer
/// Notification JSON.
fn decode_notification(raw: &[u8]) -> Result<Value, WebhookError> {
    let envelope: Value = serde_json::from_slice(raw).map_err(|e| WebhookError::Malformed(e.to_string()))?;
    let data = envelope["message"]["data"].as_str().ok_or_else(|| WebhookError::Malformed("missing message.data".into()))?;
    let decoded = B64STD.decode(data).map_err(|_| WebhookError::Malformed("message.data is not base64".into()))?;
    serde_json::from_slice(&decoded).map_err(|e| WebhookError::Malformed(format!("message.data is not JSON: {e}")))
}

fn check_package(cfg: &ProviderConfig, n: &Value) -> Result<(), WebhookError> {
    let expected = cfg.extra("package_name").filter(|s| !s.is_empty()).ok_or(WebhookError::NotConfigured)?;
    match n["packageName"].as_str() {
        Some(p) if p == expected => Ok(()),
        Some(other) => Err(WebhookError::Ignored(format!("notification for package {other}, expected {expected}"))),
        None => Err(WebhookError::Malformed("missing packageName".into())),
    }
}

/// Verify + (when mintable) re-confirm one RTDN delivery, returning the
/// deposit to mint. Non-crediting notification types — lifecycle-only
/// subscription states, `REVOKED`/`CANCELED` (handled instead by
/// [`verify_reversal_webhook`] via `voidedPurchaseNotification`), test pings —
/// are [`WebhookError::Ignored`].
pub async fn verify_webhook<H: Headers>(
    transport: Arc<dyn HttpTransport>,
    cfg: &ProviderConfig,
    headers: &H,
    raw: &[u8],
) -> Result<DepositEvent, WebhookError> {
    verify_token(cfg, headers)?;
    let n = decode_notification(raw)?;
    if !n["testNotification"].is_null() {
        return Err(WebhookError::Ignored("TEST".into()));
    }
    if !n["voidedPurchaseNotification"].is_null() {
        return Err(WebhookError::Ignored("VOIDED_PURCHASE".into()));
    }
    check_package(cfg, &n)?;

    let (kind, sku, token, type_name) = if let Some(sn) = n["subscriptionNotification"].as_object() {
        let t = sn.get("notificationType").and_then(Value::as_i64).unwrap_or(0);
        let name = subscription_type_name(t);
        if !matches!(t, 1 | 2 | 4 | 7) {
            return Err(WebhookError::Ignored(name.to_string()));
        }
        let sku = sn.get("subscriptionId").and_then(Value::as_str).unwrap_or_default().to_string();
        let token = sn.get("purchaseToken").and_then(Value::as_str).unwrap_or_default().to_string();
        (Kind::Subscription, sku, token, name)
    } else if let Some(pn) = n["oneTimeProductNotification"].as_object() {
        let t = pn.get("notificationType").and_then(Value::as_i64).unwrap_or(0);
        let name = one_time_type_name(t);
        if t != 1 {
            return Err(WebhookError::Ignored(name.to_string()));
        }
        let sku = pn.get("sku").and_then(Value::as_str).unwrap_or_default().to_string();
        let token = pn.get("purchaseToken").and_then(Value::as_str).unwrap_or_default().to_string();
        (Kind::Product, sku, token, name)
    } else {
        return Err(WebhookError::Malformed("notification has no recognized payload".into()));
    };

    if sku.trim().is_empty() || token.trim().is_empty() {
        return Err(WebhookError::Malformed(format!("{type_name} missing sku or purchaseToken")));
    }

    let ramp = PlayStoreRamp::new(transport, cfg.clone());
    let info = ramp.lookup(kind, &sku, &token).await.map_err(|_| WebhookError::Ignored(format!("{type_name}: purchase not confirmable")))?;
    if !info.paid {
        return Err(WebhookError::Ignored(format!("{type_name}: not in a paid state")));
    }
    if info.order_id.trim().is_empty() {
        return Err(WebhookError::Malformed(format!("{type_name}: provider returned no order id")));
    }
    let account_id = info.account_id.ok_or_else(|| {
        WebhookError::Malformed("missing obfuscatedExternalAccountId (set it to the Mint account at purchase)".into())
    })?;
    let amount_micros = catalog_price_micros(cfg, &sku).ok_or_else(|| WebhookError::Malformed(format!("no catalog price configured for {sku}")))?;

    Ok(DepositEvent {
        provider: ProviderId::PlayStore,
        deposit_ref: composite_ref(kind, &sku, &token, &info.order_id),
        amount_micros,
        asset: Asset::Usd,
        account_id,
    })
}

/// Verify a `voidedPurchaseNotification` as the reversal of its purchase. No
/// second Developer API call is needed: Google includes the `orderId` and
/// `productType` directly in the push body.
pub fn verify_reversal_webhook<H: Headers>(cfg: &ProviderConfig, headers: &H, raw: &[u8]) -> Result<ReversalEvent, WebhookError> {
    verify_token(cfg, headers)?;
    let n = decode_notification(raw)?;
    check_package(cfg, &n)?;
    let v = n["voidedPurchaseNotification"].as_object().ok_or_else(|| WebhookError::Ignored("not a voided-purchase notification".into()))?;
    let order_id = v.get("orderId").and_then(Value::as_str).filter(|s| !s.is_empty()).ok_or_else(|| WebhookError::Malformed("voided purchase missing orderId".into()))?;
    // productType: 1 = subscription, 2 = one-time product (Google's
    // VoidedPurchaseNotification#productType).
    let product_type = v.get("productType").and_then(Value::as_i64).unwrap_or(1);
    let kind = if product_type == 2 { Kind::Product } else { Kind::Subscription };
    let token = v.get("purchaseToken").and_then(Value::as_str).unwrap_or_default();
    // The exact composite ref isn't recoverable without the sku (not carried
    // here), so this is matched by the reserve's `alt_refs` sweep against
    // whatever `<kind>|<sku>|<token>|<order_id>` was actually minted; a
    // purchase-token-only fallback ref lets an operator trace it even when the
    // sku-qualified key doesn't match by construction.
    let fallback_ref = format!("{}|{}", kind.tag(), token);
    Ok(ReversalEvent {
        provider: ProviderId::PlayStore,
        deposit_ref: order_id.to_string(),
        alt_refs: vec![fallback_ref],
        // Play refunds (full or quantity-based partial) do not report a USD
        // amount; claw back the whole minted deposit, same stance as Apple.
        amount_micros: None,
        reason: ReversalReason::Refund,
    })
}

fn subscription_type_name(t: i64) -> &'static str {
    match t {
        1 => "SUBSCRIPTION_RECOVERED",
        2 => "SUBSCRIPTION_RENEWED",
        3 => "SUBSCRIPTION_CANCELED",
        4 => "SUBSCRIPTION_PURCHASED",
        5 => "SUBSCRIPTION_ON_HOLD",
        6 => "SUBSCRIPTION_IN_GRACE_PERIOD",
        7 => "SUBSCRIPTION_RESTARTED",
        8 => "SUBSCRIPTION_PRICE_CHANGE_CONFIRMED",
        9 => "SUBSCRIPTION_DEFERRED",
        10 => "SUBSCRIPTION_PAUSED",
        11 => "SUBSCRIPTION_PAUSE_SCHEDULE_CHANGED",
        12 => "SUBSCRIPTION_REVOKED",
        13 => "SUBSCRIPTION_EXPIRED",
        19 => "SUBSCRIPTION_PENDING_PURCHASE_CANCELED",
        _ => "SUBSCRIPTION_UNKNOWN",
    }
}

fn one_time_type_name(t: i64) -> &'static str {
    match t {
        1 => "ONE_TIME_PRODUCT_PURCHASED",
        2 => "ONE_TIME_PRODUCT_CANCELED",
        _ => "ONE_TIME_PRODUCT_UNKNOWN",
    }
}

/// Strip PEM armor + whitespace and base64-decode into the raw PKCS8 DER
/// (same approach as [`crate::iap::appstore::AppStoreRamp::api_token`]).
fn decode_pkcs8_pem(pem: &str) -> Option<Vec<u8>> {
    let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    B64STD.decode(b64.trim()).ok()
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Mode;
    use crate::http::{HttpResponse, TransportError};
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// A synthetic (not a real credential) 2048-bit PKCS8 RSA test key,
    /// generated once with `openssl genpkey -algorithm RSA | openssl pkcs8
    /// -topk8 -nocrypt`, so `access_token()`'s real RS256 signing path runs
    /// in tests exactly as it will in production (no mocked signer).
    const TEST_PKCS8_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC6ijmeYP78gMVN\n\
NmaKv+tS8IRKRIQRCWTX7vHY/vRPxf0O5fHtfALfr/EefS0TKA4uPeUMLKFBBCGM\n\
OSJVAa69cpQh8oS+lIrL3LxRYDTFBKpSCdDTSdM+Ky7R/nNoAvUT5pMIgXd5eGl9\n\
gLTOkL5eA46jx2ouuqMKVOrQD0RupMwI19+qb7gUlVcowPs95vc+mzjKegqzKSxl\n\
g3zrc5TODshxRX22xnIBeJdGGG4tfiDnHcMFLdqBPvfxqQ50JxS1Rvvm0lz4p4hr\n\
3lsb743UEisCmHYBEnHsrq6ncZ8szWnF5VAT1cr4p4TWMUQfdC0UvUgfilsVhxRn\n\
26ADgTcZAgMBAAECggEACXrdV0Ju1aEWBdNvg3p3Pd7kcOF0Wb5MY5nsggk2Rs3q\n\
dfpIdiJ7gz0t3TgM1UtPlIOHk1EQUoggdqpvp3ofVILmatGS8/GDb+f+97QSh8xc\n\
ezCtrhMzewEW+cvl4bcRbT+7oK7IW4IQ+wm9BpNot/PpKO3JOb3j68eUKwXfmy91\n\
WyoU63xN8nyISU5Eh2xguGyMF1HERWdPzIOJsSvRHDqZ8vA9MDHQexg382/eOSCc\n\
M3qCn2x8Bds0kXFPrEKDEFTJ6srgKdlHLmt9ANiCG3TsDbLvsBFNyt9WD+q5wcF4\n\
XLRaHkUdwuYjqOFczd/oXrnTCrE2NnILkkpAUIdQ1QKBgQDwyGRqOmSSJpSzwRPV\n\
G6TV31t8Vb1NYyogUZFVNIITC2cOC2Lm3S6OfQ4r67rgs0o40hHjG0loPITdliWt\n\
LrggVEGLg4/ZKoatiRqPSxBqTuvW/pXm3VRa9OQ/yBuSCyQbr4OFLGRcwjRvzw0H\n\
85udRMnSRJNjsbfh3DphnPxSZQKBgQDGVD3g4zZogMmdes+qu/DpweU4z+Hw2wRA\n\
fjJOtOzPNAGMbjxoQA72WXCsft7JQhrzs3O430H8oOGnrb9Cr9sW4IlCiE+khn1e\n\
4izheNy1Vs/HeXOgpRxXZ4O/2QfKfgpFoBx2bZ9G8ncPzLec5F2rnsTkgV4I4ijO\n\
WpQ6AQ/spQKBgAj4zpUOu/SeL2hLR3vnXcsCWZxWbYlWqpktsOal4Drpb9ucHQFR\n\
OIziqAaG8B2zUI6xdS0rNR8HMkQ3dgvv87hwq0iRjGihJweMVksElwX/DtaiAd/q\n\
W1xQR7kAsdZNR/koHxR1RQAX1HBeHLQxJZlEmOiW2Y8bbiiiViKRAO3pAoGAVJ2y\n\
fNVT7EguPMMeANEImyL/ojqHQlSfPsjYu93TPyAIHRsXKL9T4uUL/nxunt1WVG1W\n\
zSRrqdYWEI0/kbsntV39EebIXACJUscasOP4/mFYatm1bG+lZOEkQ+Dbwxf9V/FU\n\
0243y3Q9Of90wzpngIZPqNJNBWtAT7IzlX/IhIUCgYEAvsVcPZ6vYUL0gi9nCmLE\n\
HiiEQ2zFNcz9sU3DEukaY5A2UxGwSIs8/kMpMZZ49J+MDOXUVYtCbNQvu6i6hRrH\n\
oWPnIciWCPSgw07Fs1dbI3PSRwwyykYvk7unRwkysVIXAfwAnXN8jQtqD3HbSKXs\n\
K2nCTa7NqSZGARqYLSthJRM=\n\
-----END PRIVATE KEY-----\n";

    fn cfg(prices: &str) -> ProviderConfig {
        ProviderConfig {
            provider: ProviderId::PlayStore,
            api_key: Some(TEST_PKCS8_PEM.into()),
            webhook_secret: Some("push-secret".into()),
            mode: Mode::Sandbox,
            extra: HashMap::from([
                ("package_name".to_string(), "com.acme.app".to_string()),
                ("client_email".to_string(), "svc@acme.iam.gserviceaccount.com".to_string()),
                ("prices_json".to_string(), prices.to_string()),
            ]),
        }
    }

    fn hdrs(token: &str) -> HashMap<String, String> {
        [("x-playstore-token".to_string(), token.to_string())].into_iter().collect()
    }

    fn pubsub_envelope(notification: Value) -> Vec<u8> {
        let data = B64STD.encode(notification.to_string());
        json!({ "message": { "data": data, "messageId": "1", "publishTime": "2026-01-01T00:00:00Z" }, "subscription": "projects/x/subscriptions/y" })
            .to_string()
            .into_bytes()
    }

    #[derive(Default)]
    struct FakeTransport {
        get_responses: Mutex<Vec<(u16, Vec<u8>)>>,
        post_responses: Mutex<Vec<(u16, Vec<u8>)>>,
    }

    #[async_trait]
    impl HttpTransport for FakeTransport {
        async fn get(&self, _url: &str, _h: &[(String, String)]) -> Result<HttpResponse, TransportError> {
            let (status, body) = self.get_responses.lock().unwrap().remove(0);
            Ok(HttpResponse { status, body })
        }
        async fn post_json(&self, _url: &str, _h: &[(String, String)], _b: &[u8]) -> Result<HttpResponse, TransportError> {
            let (status, body) = self.post_responses.lock().unwrap().remove(0);
            Ok(HttpResponse { status, body })
        }
    }

    fn transport_with(token_body: &[u8], lookup_body: &[u8]) -> Arc<FakeTransport> {
        Arc::new(FakeTransport {
            get_responses: Mutex::new(vec![(200, lookup_body.to_vec())]),
            post_responses: Mutex::new(vec![(200, token_body.to_vec())]),
        })
    }

    #[test]
    fn tampered_token_is_rejected() {
        let n = pubsub_envelope(json!({ "testNotification": { "version": "1.0" } }));
        let transport = Arc::new(FakeTransport::default());
        let result = tokio_test_block_on(verify_webhook(transport, &cfg("{}"), &hdrs("wrong"), &n));
        assert_eq!(result, Err(WebhookError::BadSignature));
    }

    #[test]
    fn unconfigured_secret_is_not_configured() {
        let mut c = cfg("{}");
        c.webhook_secret = None;
        let n = pubsub_envelope(json!({ "testNotification": {} }));
        let transport = Arc::new(FakeTransport::default());
        let result = tokio_test_block_on(verify_webhook(transport, &c, &hdrs("anything"), &n));
        assert_eq!(result, Err(WebhookError::NotConfigured));
    }

    #[test]
    fn test_notification_is_ignored() {
        let n = pubsub_envelope(json!({ "testNotification": { "version": "1.0" } }));
        let transport = Arc::new(FakeTransport::default());
        let result = tokio_test_block_on(verify_webhook(transport, &cfg("{}"), &hdrs("push-secret"), &n));
        assert_eq!(result, Err(WebhookError::Ignored("TEST".into())));
    }

    #[test]
    fn wrong_package_is_ignored() {
        let n = pubsub_envelope(json!({ "packageName": "com.other.app", "subscriptionNotification": { "notificationType": 4, "purchaseToken": "t", "subscriptionId": "pro" } }));
        let transport = Arc::new(FakeTransport::default());
        let result = tokio_test_block_on(verify_webhook(transport, &cfg("{}"), &hdrs("push-secret"), &n));
        assert!(matches!(result, Err(WebhookError::Ignored(_))));
    }

    #[test]
    fn purchased_subscription_mints_the_catalog_price_under_the_looked_up_order_id() {
        let n = pubsub_envelope(json!({
            "packageName": "com.acme.app",
            "subscriptionNotification": { "version": "1.0", "notificationType": 4, "purchaseToken": "ptok_1", "subscriptionId": "pro_monthly" }
        }));
        let token_resp = json!({ "access_token": "tok", "expires_in": 3599, "token_type": "Bearer" }).to_string().into_bytes();
        let lookup_resp = json!({
            "subscriptionState": "SUBSCRIPTION_STATE_ACTIVE",
            "latestOrderId": "GPA.3340-0000-0000-00001",
            "externalAccountIdentifiers": { "obfuscatedExternalAccountId": "acct_ada" }
        })
        .to_string()
        .into_bytes();
        let transport = transport_with(&token_resp, &lookup_resp);
        let result = tokio_test_block_on(verify_webhook(transport, &cfg(r#"{"pro_monthly":499}"#), &hdrs("push-secret"), &n)).expect("mints");
        assert_eq!(result.amount_micros, 4_990_000);
        assert_eq!(result.account_id, "acct_ada");
        assert_eq!(result.deposit_ref, "sub|pro_monthly|ptok_1|GPA.3340-0000-0000-00001");
        assert_eq!(result.namespaced_ref(), "playstore:sub|pro_monthly|ptok_1|GPA.3340-0000-0000-00001");
    }

    #[test]
    fn purchased_one_time_product_mints_under_product_lookup() {
        let n = pubsub_envelope(json!({
            "packageName": "com.acme.app",
            "oneTimeProductNotification": { "version": "1.0", "notificationType": 1, "purchaseToken": "ptok_2", "sku": "credits_10" }
        }));
        let token_resp = json!({ "access_token": "tok" }).to_string().into_bytes();
        let lookup_resp = json!({ "purchaseState": 0, "orderId": "GPA.1-ONE-TIME-0001", "obfuscatedExternalAccountId": "acct_bea" }).to_string().into_bytes();
        let transport = transport_with(&token_resp, &lookup_resp);
        let result = tokio_test_block_on(verify_webhook(transport, &cfg(r#"{"credits_10":999}"#), &hdrs("push-secret"), &n)).expect("mints");
        assert_eq!(result.amount_micros, 9_990_000);
        assert_eq!(result.deposit_ref, "prod|credits_10|ptok_2|GPA.1-ONE-TIME-0001");
    }

    #[test]
    fn renewal_mints_under_a_different_order_id_than_the_original_purchase() {
        // Same purchaseToken (Play keeps it stable across a subscription's own
        // renewals), but the Developer API reports a NEW latestOrderId — this
        // is exactly the real-world shape a RENEWED notification has, and the
        // whole point of keying `deposit_ref` on order id instead of token:
        // the renewal must mint again, not replay the original purchase.
        let original = pubsub_envelope(json!({
            "packageName": "com.acme.app",
            "subscriptionNotification": { "notificationType": 4, "purchaseToken": "ptok_3", "subscriptionId": "pro_monthly" }
        }));
        let renewal = pubsub_envelope(json!({
            "packageName": "com.acme.app",
            "subscriptionNotification": { "notificationType": 2, "purchaseToken": "ptok_3", "subscriptionId": "pro_monthly" }
        }));
        let token_resp = || json!({ "access_token": "tok" }).to_string().into_bytes();
        let lookup = |order: &str| {
            json!({ "subscriptionState": "SUBSCRIPTION_STATE_ACTIVE", "latestOrderId": order, "externalAccountIdentifiers": { "obfuscatedExternalAccountId": "acct_ada" } })
                .to_string()
                .into_bytes()
        };
        let t1 = transport_with(&token_resp(), &lookup("GPA.3340-0000-0000-00001"));
        let r1 = tokio_test_block_on(verify_webhook(t1, &cfg(r#"{"pro_monthly":499}"#), &hdrs("push-secret"), &original)).expect("mints");
        let t2 = transport_with(&token_resp(), &lookup("GPA.3340-0000-0000-00002"));
        let r2 = tokio_test_block_on(verify_webhook(t2, &cfg(r#"{"pro_monthly":499}"#), &hdrs("push-secret"), &renewal)).expect("mints");
        assert_ne!(r1.namespaced_ref(), r2.namespaced_ref());
    }

    #[test]
    fn missing_account_binding_is_malformed() {
        let n = pubsub_envelope(json!({
            "packageName": "com.acme.app",
            "oneTimeProductNotification": { "notificationType": 1, "purchaseToken": "ptok_4", "sku": "credits_10" }
        }));
        let token_resp = json!({ "access_token": "tok" }).to_string().into_bytes();
        let lookup_resp = json!({ "purchaseState": 0, "orderId": "GPA.1-NOACCT" }).to_string().into_bytes();
        let transport = transport_with(&token_resp, &lookup_resp);
        let result = tokio_test_block_on(verify_webhook(transport, &cfg(r#"{"credits_10":999}"#), &hdrs("push-secret"), &n));
        assert!(matches!(result, Err(WebhookError::Malformed(_))));
    }

    #[test]
    fn unpaid_purchase_state_is_ignored() {
        let n = pubsub_envelope(json!({
            "packageName": "com.acme.app",
            "oneTimeProductNotification": { "notificationType": 1, "purchaseToken": "ptok_5", "sku": "credits_10" }
        }));
        let token_resp = json!({ "access_token": "tok" }).to_string().into_bytes();
        // purchaseState 1 == "canceled".
        let lookup_resp = json!({ "purchaseState": 1, "orderId": "GPA.1-CANCELED", "obfuscatedExternalAccountId": "acct_x" }).to_string().into_bytes();
        let transport = transport_with(&token_resp, &lookup_resp);
        let result = tokio_test_block_on(verify_webhook(transport, &cfg(r#"{"credits_10":999}"#), &hdrs("push-secret"), &n));
        assert!(matches!(result, Err(WebhookError::Ignored(_))));
    }

    #[test]
    fn lifecycle_only_subscription_notifications_are_ignored_without_a_lookup() {
        // ON_HOLD (5) must never trigger a Developer API call (no responses
        // queued ⇒ a lookup attempt would panic the fake transport).
        let n = pubsub_envelope(json!({
            "packageName": "com.acme.app",
            "subscriptionNotification": { "notificationType": 5, "purchaseToken": "ptok_6", "subscriptionId": "pro_monthly" }
        }));
        let transport = Arc::new(FakeTransport::default());
        let result = tokio_test_block_on(verify_webhook(transport, &cfg("{}"), &hdrs("push-secret"), &n));
        assert_eq!(result, Err(WebhookError::Ignored("SUBSCRIPTION_ON_HOLD".into())));
    }

    #[test]
    fn revoked_subscription_is_ignored_not_reversed_here() {
        let n = pubsub_envelope(json!({
            "packageName": "com.acme.app",
            "subscriptionNotification": { "notificationType": 12, "purchaseToken": "ptok_7", "subscriptionId": "pro_monthly" }
        }));
        let transport = Arc::new(FakeTransport::default());
        let result = tokio_test_block_on(verify_webhook(transport, &cfg("{}"), &hdrs("push-secret"), &n));
        assert_eq!(result, Err(WebhookError::Ignored("SUBSCRIPTION_REVOKED".into())));
    }

    #[test]
    fn voided_purchase_reverses_by_order_id_with_no_network_call() {
        let n = pubsub_envelope(json!({
            "packageName": "com.acme.app",
            "voidedPurchaseNotification": { "purchaseToken": "ptok_8", "orderId": "GPA.3340-0000-0000-00001", "productType": 1, "refundType": 1 }
        }));
        let rev = verify_reversal_webhook(&cfg("{}"), &hdrs("push-secret"), &n).expect("reverses");
        assert_eq!(rev.deposit_ref, "GPA.3340-0000-0000-00001");
        assert_eq!(rev.namespaced_ref(), "playstore:GPA.3340-0000-0000-00001");
        assert_eq!(rev.amount_micros, None);
        assert_eq!(rev.alt_refs, vec!["sub|ptok_8".to_string()]);
    }

    #[test]
    fn tampered_token_rejects_the_reversal_too() {
        let n = pubsub_envelope(json!({ "voidedPurchaseNotification": { "orderId": "x", "purchaseToken": "y", "productType": 1 } }));
        let result = verify_reversal_webhook(&cfg("{}"), &hdrs("wrong"), &n);
        assert_eq!(result, Err(WebhookError::BadSignature));
    }

    #[tokio::test]
    async fn confirm_deposit_for_requires_matching_account_and_order_id() {
        let token_resp = json!({ "access_token": "tok" }).to_string().into_bytes();
        let lookup_resp = json!({ "subscriptionState": "SUBSCRIPTION_STATE_ACTIVE", "latestOrderId": "GPA.OK", "externalAccountIdentifiers": { "obfuscatedExternalAccountId": "acct_ada" } })
            .to_string()
            .into_bytes();
        let transport = transport_with(&token_resp, &lookup_resp);
        let ramp = PlayStoreRamp::new(transport, cfg(r#"{"pro_monthly":499}"#));
        let ref_ = composite_ref(Kind::Subscription, "pro_monthly", "ptok", "GPA.OK");
        let dep = ramp.confirm_deposit_for(&ref_, "acct_ada", Micros::new(4_990_000)).await.expect("confirmed");
        assert_eq!(dep.amount_micros, Micros::new(4_990_000));
        assert_eq!(dep.bound_account.as_deref(), Some("acct_ada"));
    }

    #[tokio::test]
    async fn confirm_deposit_for_refuses_a_different_account() {
        let token_resp = json!({ "access_token": "tok" }).to_string().into_bytes();
        let lookup_resp = json!({ "subscriptionState": "SUBSCRIPTION_STATE_ACTIVE", "latestOrderId": "GPA.OK", "externalAccountIdentifiers": { "obfuscatedExternalAccountId": "acct_ada" } })
            .to_string()
            .into_bytes();
        let transport = transport_with(&token_resp, &lookup_resp);
        let ramp = PlayStoreRamp::new(transport, cfg(r#"{"pro_monthly":499}"#));
        let ref_ = composite_ref(Kind::Subscription, "pro_monthly", "ptok", "GPA.OK");
        let result = ramp.confirm_deposit_for(&ref_, "acct_mallory", Micros::new(4_990_000)).await;
        assert_eq!(result, Err(RampError::NotConfirmed));
    }

    #[tokio::test]
    async fn confirm_deposit_for_refuses_a_stale_order_id() {
        let token_resp = json!({ "access_token": "tok" }).to_string().into_bytes();
        // Developer API now reports a NEWER order id than the one this ref
        // was minted under — the ref's own order already happened, but it is
        // no longer the CURRENT one; fail closed rather than reconfirm.
        let lookup_resp = json!({ "subscriptionState": "SUBSCRIPTION_STATE_ACTIVE", "latestOrderId": "GPA.NEWER", "externalAccountIdentifiers": { "obfuscatedExternalAccountId": "acct_ada" } })
            .to_string()
            .into_bytes();
        let transport = transport_with(&token_resp, &lookup_resp);
        let ramp = PlayStoreRamp::new(transport, cfg(r#"{"pro_monthly":499}"#));
        let ref_ = composite_ref(Kind::Subscription, "pro_monthly", "ptok", "GPA.OLDER");
        let result = ramp.confirm_deposit_for(&ref_, "acct_ada", Micros::new(4_990_000)).await;
        assert_eq!(result, Err(RampError::NotConfirmed));
    }

    #[tokio::test]
    async fn confirm_deposit_is_not_supported_without_a_composite_ref() {
        let transport = Arc::new(FakeTransport::default());
        let ramp = PlayStoreRamp::new(transport, cfg("{}"));
        let result = ramp.confirm_deposit("GPA.some-order-id").await;
        assert_eq!(result, Err(RampError::DepositNotFound));
    }

    #[tokio::test]
    async fn no_payout_supported() {
        let transport = Arc::new(FakeTransport::default());
        let ramp = PlayStoreRamp::new(transport, cfg("{}"));
        let dest = PayoutDest::Stripe { account: "acct".into() };
        assert_eq!(ramp.initiate_payout("playstore:1", &dest, Micros::new(1)).await, Err(RampError::ProviderUnavailable));
        assert_eq!(ramp.check_payout("playstore:1", &dest, Micros::new(1)).await, Err(RampError::Unsupported));
    }

    /// Run a `Future` to completion without pulling `#[tokio::test]` onto the
    /// sync tests that assert BEFORE any network call would happen (keeps the
    /// "no responses queued ⇒ would panic" guard in
    /// `lifecycle_only_subscription_notifications_are_ignored_without_a_lookup`
    /// meaningful — a `#[tokio::test]` runtime wouldn't change that, this is
    /// just the lightest block-on available without a new dev-dependency).
    fn tokio_test_block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
    }
}
