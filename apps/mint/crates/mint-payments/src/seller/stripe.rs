//! The seller's own Stripe account through Stripe Connect (Wave P PY2.3).
//! Off unless the operator configures Connect (owner decision OD-9):
//! `MINT_STRIPE_CONNECT_CLIENT_ID`, `MINT_STRIPE_PLATFORM_SECRET_KEY`,
//! `MINT_STRIPE_CONNECT_WEBHOOK_SECRET`. The seller authorizes on Stripe's
//! own page (OAuth); Mint keeps only the connected account id and acts on that
//! account with the `Stripe-Account` header. Charges are **direct charges on
//! the seller's account**: no application fee, no transfer, nothing held.

use serde_json::Value;

use super::{meta_from, CheckoutMeta, CheckoutSession, EntitlementEvent, EntitlementKind, Interval, ProductSpec, RevokeReason, SellerError};
use crate::card::stripe::{form_urlencode, parse_stripe_sig};
use crate::http::{Headers, HttpTransport};
use crate::verify::hmac_sha256_hex_verify;
use crate::WebhookError;

const API: &str = "https://api.stripe.com";
const CONNECT: &str = "https://connect.stripe.com";
pub const TOLERANCE_SECS: i64 = 300;

/// The operator's Connect settings. `Debug` shows only the client id.
#[derive(Clone)]
pub struct ConnectConfig {
    pub client_id: String,
    pub platform_secret: String,
    pub webhook_secret: String,
}

impl std::fmt::Debug for ConnectConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectConfig").field("client_id", &self.client_id).finish_non_exhaustive()
    }
}

fn pct(s: &str) -> String {
    form_urlencode(&[("x", s)])[2..].to_owned()
}

/// Stripe's authorize page for the seller (`state` is Mint's one-time connect token).
pub fn authorize_url(cfg: &ConnectConfig, state: &str, redirect_uri: &str) -> String {
    format!(
        "{CONNECT}/oauth/authorize?response_type=code&scope=read_write&client_id={}&state={}&redirect_uri={}",
        pct(&cfg.client_id),
        pct(state),
        pct(redirect_uri)
    )
}

fn headers(cfg: &ConnectConfig, account: Option<&str>) -> Vec<(String, String)> {
    let mut h = vec![
        ("authorization".into(), format!("Bearer {}", cfg.platform_secret)),
        ("content-type".into(), "application/x-www-form-urlencoded".into()),
    ];
    if let Some(acct) = account {
        h.push(("stripe-account".into(), acct.to_owned()));
    }
    h
}

fn answer(status: u16, body: &[u8]) -> Result<Value, SellerError> {
    match status {
        200..=299 => serde_json::from_slice(body).map_err(|_| SellerError::Unavailable),
        400..=499 => {
            let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
            let msg = v["error"]["message"].as_str().or_else(|| v["error_description"].as_str()).unwrap_or("invalid request");
            Err(SellerError::Refused(msg.chars().take(200).collect()))
        }
        _ => Err(SellerError::Unavailable),
    }
}

async fn post(transport: &dyn HttpTransport, cfg: &ConnectConfig, account: Option<&str>, url: &str, form: &[(&str, &str)]) -> Result<Value, SellerError> {
    let body = form_urlencode(form);
    let resp = transport.post_json(url, &headers(cfg, account), body.as_bytes()).await.map_err(|_| SellerError::Unavailable)?;
    answer(resp.status, &resp.body)
}

fn valid_account(acct: &str) -> bool {
    acct.starts_with("acct_") && acct.len() <= 64 && acct.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Exchanges the OAuth code for the seller's connected account id.
///
/// # Errors
/// A refusal (bad or used code) or Stripe not answering.
pub async fn exchange_code(transport: &dyn HttpTransport, cfg: &ConnectConfig, code: &str) -> Result<String, SellerError> {
    let v = post(transport, cfg, None, &format!("{CONNECT}/oauth/token"), &[("grant_type", "authorization_code"), ("code", code)]).await?;
    v["stripe_user_id"].as_str().filter(|a| valid_account(a)).map(str::to_owned).ok_or(SellerError::Unavailable)
}

/// Creates the product and its price on the connected account; returns the price id.
///
/// # Errors
/// Invalid spec, a refusal, or Stripe not answering.
pub async fn create_product(transport: &dyn HttpTransport, cfg: &ConnectConfig, account: &str, spec: &ProductSpec) -> Result<String, SellerError> {
    spec.validate().map_err(SellerError::Invalid)?;
    if !valid_account(account) {
        return Err(SellerError::NotConnected);
    }
    let product = post(transport, cfg, Some(account), &format!("{API}/v1/products"), &[("name", &spec.name), ("metadata[sku]", &spec.sku)]).await?;
    let product_id = product["id"].as_str().ok_or(SellerError::Unavailable)?.to_owned();
    let cents = spec.price_cents.to_string();
    let currency = spec.currency.to_ascii_lowercase();
    let mut form = vec![("product", product_id.as_str()), ("unit_amount", cents.as_str()), ("currency", currency.as_str())];
    if let Some(interval) = spec.interval {
        form.push(("recurring[interval]", if interval == Interval::Month { "month" } else { "year" }));
    }
    let price = post(transport, cfg, Some(account), &format!("{API}/v1/prices"), &form).await?;
    price["id"].as_str().filter(|id| id.starts_with("price_")).map(str::to_owned).ok_or(SellerError::Unavailable)
}

/// What to sell in one Checkout Session and where the buyer returns.
#[derive(Clone, Copy, Debug)]
pub struct CheckoutRequest<'a> {
    pub price_id: &'a str,
    pub recurring: bool,
    pub success_url: &'a str,
    pub cancel_url: &'a str,
}

/// Opens a Checkout Session on the connected account (a direct charge).
///
/// # Errors
/// A refusal, or Stripe not answering.
pub async fn create_checkout(
    transport: &dyn HttpTransport,
    cfg: &ConnectConfig,
    account: &str,
    request: &CheckoutRequest<'_>,
    meta: &CheckoutMeta<'_>,
) -> Result<CheckoutSession, SellerError> {
    let CheckoutRequest { price_id, recurring, success_url, cancel_url } = *request;
    if !valid_account(account) {
        return Err(SellerError::NotConnected);
    }
    let copy = if recurring { "subscription_data[metadata]" } else { "payment_intent_data[metadata]" };
    let keys = [format!("{copy}[mint_seller]"), format!("{copy}[buyer_ref]"), format!("{copy}[sku]")];
    let form: Vec<(&str, &str)> = vec![
        ("mode", if recurring { "subscription" } else { "payment" }),
        ("line_items[0][price]", price_id),
        ("line_items[0][quantity]", "1"),
        ("success_url", success_url),
        ("cancel_url", cancel_url),
        ("client_reference_id", meta.buyer_ref),
        ("metadata[mint_seller]", meta.seller_key),
        ("metadata[buyer_ref]", meta.buyer_ref),
        ("metadata[sku]", meta.sku),
        (&keys[0], meta.seller_key),
        (&keys[1], meta.buyer_ref),
        (&keys[2], meta.sku),
    ];
    let v = post(transport, cfg, Some(account), &format!("{API}/v1/checkout/sessions"), &form).await?;
    match (v["url"].as_str(), v["id"].as_str()) {
        (Some(url), Some(id)) if url.starts_with("https://") => Ok(CheckoutSession { url: url.to_owned(), id: id.to_owned() }),
        _ => Err(SellerError::Unavailable),
    }
}

/// A verified Connect event: the connected account it came from and what it
/// means (`None` when it changes no entitlement).
#[derive(Debug)]
pub struct ConnectEvent {
    pub account: String,
    pub event: Option<EntitlementEvent>,
    /// `account.application.deauthorized`: the seller disconnected our
    /// platform from their Stripe account; nothing can be sold on it anymore.
    pub deauthorized: bool,
}

/// Verifies one Connect delivery (the platform's endpoint secret) and maps it.
///
/// # Errors
/// `BadSignature` / `NotConfigured` / `Malformed`.
pub async fn verify_event<H: Headers>(
    transport: &dyn HttpTransport,
    cfg: &ConnectConfig,
    headers_in: &H,
    raw: &[u8],
    now_secs: i64,
) -> Result<ConnectEvent, WebhookError> {
    if cfg.webhook_secret.is_empty() {
        return Err(WebhookError::NotConfigured);
    }
    let sig = headers_in.get_header("stripe-signature").ok_or(WebhookError::BadSignature)?;
    let (t, v1) = parse_stripe_sig(sig).ok_or(WebhookError::BadSignature)?;
    let ts: i64 = t.parse().map_err(|_| WebhookError::BadSignature)?;
    if (now_secs - ts).abs() > TOLERANCE_SECS {
        return Err(WebhookError::BadSignature);
    }
    let mut message = Vec::with_capacity(t.len() + 1 + raw.len());
    message.extend_from_slice(t.as_bytes());
    message.push(b'.');
    message.extend_from_slice(raw);
    hmac_sha256_hex_verify(cfg.webhook_secret.as_bytes(), &message, v1)?;
    let v: Value = serde_json::from_slice(raw).map_err(|_| WebhookError::Malformed("not JSON".into()))?;
    let account = v["account"].as_str().filter(|a| valid_account(a)).ok_or_else(|| WebhookError::Malformed("not a Connect event".into()))?.to_owned();
    let object = &v["data"]["object"];
    if v["type"].as_str() == Some("account.application.deauthorized") {
        return Ok(ConnectEvent { account, event: None, deauthorized: true });
    }
    let (kind, reason, meta) = match v["type"].as_str().unwrap_or("") {
        "checkout.session.completed" if matches!(object["payment_status"].as_str(), Some("paid" | "no_payment_required")) => {
            (EntitlementKind::Granted, None, object["metadata"].clone())
        }
        "invoice.paid" if object["billing_reason"].as_str() == Some("subscription_cycle") => {
            (EntitlementKind::Granted, None, object["subscription_details"]["metadata"].clone())
        }
        "customer.subscription.deleted" => (EntitlementKind::Revoked, Some(RevokeReason::Cancelled), object["metadata"].clone()),
        kind @ ("charge.refunded" | "charge.dispute.created") => {
            let pi = object["payment_intent"].as_str().ok_or_else(|| WebhookError::Malformed("no payment_intent".into()))?;
            if !pi.starts_with("pi_") || !pi.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
                return Err(WebhookError::Malformed("bad payment_intent".into()));
            }
            let resp = transport
                .get(&format!("{API}/v1/payment_intents/{pi}"), &headers(cfg, Some(&account)))
                .await
                .map_err(|_| WebhookError::Malformed("payment lookup failed".into()))?;
            let intent = answer(resp.status, &resp.body).map_err(|_| WebhookError::Malformed("payment lookup failed".into()))?;
            let reason = if kind == "charge.refunded" { RevokeReason::Refund } else { RevokeReason::Dispute };
            (EntitlementKind::Revoked, Some(reason), intent["metadata"].clone())
        }
        _ => return Ok(ConnectEvent { account, event: None, deauthorized: false }),
    };
    let event = meta_from(&meta).map(|(seller_key, buyer_ref, sku)| EntitlementEvent {
        kind,
        reason,
        seller_key,
        buyer_ref,
        sku,
        provider_event_id: format!("stripe:{}", v["id"].as_str().unwrap_or_default()),
        occurred_at_ms: v["created"].as_i64().unwrap_or(ts) * 1000,
        expires_at_ms: object["lines"]["data"][0]["period"]["end"].as_i64().map(|s| s * 1000),
    });
    Ok(ConnectEvent { account, event, deauthorized: false })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use async_trait::async_trait;
    use hmac::{Hmac, Mac};
    use serde_json::json;
    use sha2::Sha256;

    use super::{authorize_url, create_checkout, create_product, exchange_code, verify_event, CheckoutRequest, ConnectConfig};
    use crate::http::{HttpResponse, HttpTransport, TransportError};
    use crate::seller::{CheckoutMeta, EntitlementKind, Interval, ProductSpec, RevokeReason};

    type Call = (String, Vec<(String, String)>, String);

    #[derive(Default)]
    struct Fake {
        calls: Mutex<Vec<Call>>,
        answers: Mutex<Vec<(u16, String)>>,
    }

    #[async_trait]
    impl HttpTransport for Fake {
        async fn get(&self, url: &str, headers: &[(String, String)]) -> Result<HttpResponse, TransportError> {
            self.calls.lock().unwrap().push((url.into(), headers.to_vec(), String::new()));
            let (status, body) = self.answers.lock().unwrap().remove(0);
            Ok(HttpResponse { status, body: body.into_bytes() })
        }
        async fn post_json(&self, url: &str, headers: &[(String, String)], body: &[u8]) -> Result<HttpResponse, TransportError> {
            self.calls.lock().unwrap().push((url.into(), headers.to_vec(), String::from_utf8_lossy(body).into()));
            let (status, body) = self.answers.lock().unwrap().remove(0);
            Ok(HttpResponse { status, body: body.into_bytes() })
        }
    }

    fn cfg() -> ConnectConfig {
        ConnectConfig { client_id: "ca_test".into(), platform_secret: "sk_test_platform".into(), webhook_secret: "whsec_connect".into() }
    }

    fn signed(body: &serde_json::Value, ts: i64) -> (HashMap<String, String>, Vec<u8>) {
        let raw = serde_json::to_vec(body).unwrap();
        let mut mac = Hmac::<Sha256>::new_from_slice(b"whsec_connect").unwrap();
        mac.update(format!("{ts}.").as_bytes());
        mac.update(&raw);
        let sig = hex::encode(mac.finalize().into_bytes());
        (HashMap::from([("stripe-signature".to_owned(), format!("t={ts},v1={sig}"))]), raw)
    }

    #[tokio::test]
    async fn connect_then_sell_on_the_connected_account_with_direct_charges() {
        let fake = Fake::default();
        fake.answers.lock().unwrap().extend([
            (200, json!({"stripe_user_id":"acct_123"}).to_string()),
            (200, json!({"id":"prod_1"}).to_string()),
            (200, json!({"id":"price_1"}).to_string()),
            (200, json!({"id":"cs_1","url":"https://checkout.stripe.com/c/cs_1"}).to_string()),
        ]);
        let url = authorize_url(&cfg(), "tok 1", "https://mint.example/mint/connect/stripe/callback");
        assert!(url.starts_with("https://connect.stripe.com/oauth/authorize?") && url.contains("state=tok%201"));
        assert_eq!(exchange_code(&fake, &cfg(), "ac_1").await.unwrap(), "acct_123");
        let spec = ProductSpec { sku: "pro".into(), name: "Pro".into(), price_cents: 900, currency: "USD".into(), interval: Some(Interval::Year) };
        assert_eq!(create_product(&fake, &cfg(), "acct_123", &spec).await.unwrap(), "price_1");
        let meta = CheckoutMeta { seller_key: "scribase:org.shop.production", buyer_ref: "user_1", sku: "pro" };
        let req = CheckoutRequest { price_id: "price_1", recurring: true, success_url: "https://a.example/ok", cancel_url: "https://a.example/no" };
        let s = create_checkout(&fake, &cfg(), "acct_123", &req, &meta).await.unwrap();
        assert_eq!(s.id, "cs_1");
        let calls = fake.calls.lock().unwrap();
        assert!(calls[1..].iter().all(|c| c.1.contains(&("stripe-account".to_owned(), "acct_123".to_owned()))), "on the seller's account");
        assert!(calls[2].2.contains("recurring%5Binterval%5D=year"));
        assert!(calls[3].2.contains("subscription_data%5Bmetadata%5D%5Bbuyer_ref%5D=user_1"));
        assert!(!calls[3].2.contains("application_fee") && !calls[3].2.contains("transfer_data"), "no fee, no transfer: nothing reaches us");
    }

    #[tokio::test]
    async fn connect_events_map_by_the_metadata_and_carry_the_account() {
        let fake = Fake::default();
        let meta = json!({"mint_seller":"scribase:org.shop.production","buyer_ref":"user_1","sku":"pro"});
        let now = 1_790_683_200;
        let (h, raw) = signed(&json!({"id":"evt_1","type":"checkout.session.completed","account":"acct_123","created":now,"data":{"object":{"payment_status":"paid","metadata":meta}}}), now);
        let e = verify_event(&fake, &cfg(), &h, &raw, now).await.unwrap();
        assert_eq!(e.account, "acct_123");
        let ev = e.event.unwrap();
        assert_eq!((ev.kind, ev.provider_event_id.as_str()), (EntitlementKind::Granted, "stripe:evt_1"));
        fake.answers.lock().unwrap().push((200, json!({"id":"pi_1","metadata":meta}).to_string()));
        let (h, raw) = signed(&json!({"id":"evt_2","type":"charge.refunded","account":"acct_123","created":now,"data":{"object":{"payment_intent":"pi_1"}}}), now);
        let ev = verify_event(&fake, &cfg(), &h, &raw, now).await.unwrap().event.unwrap();
        assert_eq!((ev.kind, ev.reason), (EntitlementKind::Revoked, Some(RevokeReason::Refund)));
        let (mut h, raw) = signed(&json!({"id":"evt_3","type":"checkout.session.completed","account":"acct_123","data":{"object":{"payment_status":"paid","metadata":meta}}}), now);
        h.insert("stripe-signature".into(), format!("t={now},v1=00"));
        assert!(verify_event(&fake, &cfg(), &h, &raw, now).await.is_err());
        let (h, raw) = signed(&json!({"id":"evt_4","type":"checkout.session.completed","data":{"object":{}}}), now);
        assert!(verify_event(&fake, &cfg(), &h, &raw, now).await.is_err(), "not a Connect event");
        assert!(!format!("{:?}", cfg()).contains("sk_test_platform"));
    }

    #[tokio::test]
    async fn a_deauthorization_names_the_account_and_no_entitlement() {
        let fake = Fake::default();
        let now = 1_790_683_200;
        let (h, raw) = signed(
            &json!({"id":"evt_9","type":"account.application.deauthorized","account":"acct_123","created":now,"data":{"object":{"id":"ca_test"}}}),
            now,
        );
        let e = verify_event(&fake, &cfg(), &h, &raw, now).await.unwrap();
        assert!(e.deauthorized);
        assert_eq!(e.account, "acct_123");
        assert!(e.event.is_none());
        assert!(fake.calls.lock().unwrap().is_empty(), "no lookup on the revoked account");
    }
}
