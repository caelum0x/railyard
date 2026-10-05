//! Wave P PY2.3: seller-own payments end to end. A seller connects their own
//! Dodo account on the connect page (the calling product never sees a key),
//! a product and a checkout are made on the seller's account, the seller's
//! processor webhooks become signed entitlement events for the calling
//! product, one seller never reaches another, and the Mint ledger is never
//! touched (no account, no journal entry).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use mint_payments::outbound::sign;
use mint_payments::seller::seal::SealKey;
use mint_payments::{HttpResponse, HttpTransport, TransportError};
use mint_protocol::InMemoryPayerKeys;
use mint_receipts::FixedSigner;
use mint_reserve::{FakeKycGate, FakeRamp, KycTier};
use mint_server::seller::{self, ProductKey, SellerState};
use mint_server::{build_router, AppState};
use mint_store::{Clock, FixedClock, InMemoryStore, MemorySellerStore};
use serde_json::{json, Value};
use tower::ServiceExt;

const SCRIBASE_KEY: &str = "scribase-seller-key-0123456789";
const RALLY_KEY: &str = "rally-seller-key-0123456789abcd";
const PRODUCT_SECRET: &str = "whsec_cHJvZHVjdC13ZWJob29rLXNlY3JldC0zMi1ieXRlcw==";
const SELLER_HOOK: &str = "whsec_c2VsbGVyLXdlYmhvb2stc2VjcmV0LTMyLWJ5dGVzLTAwMA==";
const SEAL: &str = "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=";
const SELLER: &str = "org_1.shop.production";
const NOW_MS: i64 = 1_790_683_200_000;

type Call = (String, String, Vec<(String, String)>, Vec<u8>);

/// Answers by URL prefix, in order; records every call.
#[derive(Default)]
struct Fake {
    calls: Mutex<Vec<Call>>,
    answers: Mutex<VecDeque<(String, u16, String)>>,
}

impl Fake {
    fn answer(&self, prefix: &str, status: u16, body: Value) {
        self.answers.lock().unwrap().push_back((prefix.to_owned(), status, body.to_string()));
    }
    fn reply(&self, url: &str) -> HttpResponse {
        let mut answers = self.answers.lock().unwrap();
        let at = answers.iter().position(|(p, _, _)| url.starts_with(p.as_str()));
        match at.and_then(|i| answers.remove(i)) {
            Some((_, status, body)) => HttpResponse { status, body: body.into_bytes() },
            None => HttpResponse { status: 200, body: b"{}".to_vec() },
        }
    }
    fn calls_to(&self, prefix: &str) -> Vec<Call> {
        self.calls.lock().unwrap().iter().filter(|c| c.1.starts_with(prefix)).cloned().collect()
    }
}

#[async_trait]
impl HttpTransport for Fake {
    async fn get(&self, url: &str, headers: &[(String, String)]) -> Result<HttpResponse, TransportError> {
        self.calls.lock().unwrap().push(("GET".into(), url.into(), headers.to_vec(), Vec::new()));
        Ok(self.reply(url))
    }
    async fn post_json(&self, url: &str, headers: &[(String, String)], body: &[u8]) -> Result<HttpResponse, TransportError> {
        self.calls.lock().unwrap().push(("POST".into(), url.into(), headers.to_vec(), body.to_vec()));
        Ok(self.reply(url))
    }
}

fn now() -> i64 {
    NOW_MS
}

fn state(transport: Arc<Fake>) -> SellerState {
    SellerState {
        store: Arc::new(MemorySellerStore::default()),
        products: Arc::new(vec![
            ProductKey { product: "scribase".into(), key: SCRIBASE_KEY.into(), webhook_url: "https://api.scribase.example/v1/railyard/billing/webhooks".into(), webhook_secret: PRODUCT_SECRET.into() },
            ProductKey { product: "rally".into(), key: RALLY_KEY.into(), webhook_url: "https://rally.example/mint".into(), webhook_secret: PRODUCT_SECRET.into() },
        ]),
        seal: Some(Arc::new(SealKey::from_base64(SEAL).unwrap())),
        transport,
        public_url: "https://mint.example".into(),
        stripe: None,
        now_ms: now,
    }
}

async fn send(app: &Router, method: &str, uri: &str, key: Option<&str>, content_type: &str, body: Vec<u8>, extra: &[(&str, String)]) -> (StatusCode, String) {
    let mut req = Request::builder().method(method).uri(uri).header("content-type", content_type);
    if let Some(k) = key {
        req = req.header("x-api-key", k);
    }
    for (k, v) in extra {
        req = req.header(*k, v);
    }
    let res = app.clone().oneshot(req.body(Body::from(body)).unwrap()).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn api(app: &Router, method: &str, uri: &str, key: Option<&str>, body: Value) -> (StatusCode, Value) {
    let raw = if body.is_null() { Vec::new() } else { body.to_string().into_bytes() };
    let (status, text) = send(app, method, uri, key, "application/json", raw, &[]).await;
    (status, serde_json::from_str(&text).unwrap_or(Value::Null))
}

fn signed_seller_hook(body: &Value, id: &str) -> (Vec<u8>, Vec<(&'static str, String)>) {
    let raw = serde_json::to_vec(body).unwrap();
    let ts = (NOW_MS / 1000).to_string();
    let sig = sign(SELLER_HOOK, id, &ts, &raw).unwrap();
    (raw, vec![("webhook-id", id.to_owned()), ("webhook-timestamp", ts), ("webhook-signature", sig)])
}

/// Connects SELLER's Dodo account through the page; returns the connect token.
async fn connect_dodo(app: &Router, fake: &Fake) -> String {
    let (status, body) = api(app, "POST", &format!("/mint/v1/sellers/{SELLER}/connect"), Some(SCRIBASE_KEY), json!({"return_url": "https://console.scribase.example/back"})).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let url = body["connect_url"].as_str().unwrap().to_owned();
    assert!(url.starts_with("https://mint.example/mint/connect/"));
    let token = url.rsplit('/').next().unwrap().to_owned();
    let (status, html) = send(app, "GET", &format!("/mint/connect/{token}"), None, "text/html", Vec::new(), &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!html.split("<details").next().unwrap().contains("type=\"password\""), "no key field on the default screen");
    assert!(
        html.contains(&format!("https://mint.example/mint/webhooks/seller/scribase/{SELLER}/dodo")),
        "the webhook address is shown before its secret is asked for"
    );
    fake.answer("https://test.dodopayments.com/products?", 200, json!({"items": []}));
    let form = format!("api_key=seller_dodo_key_123&webhook_secret={}&mode=test", SELLER_HOOK.replace('=', "%3D"));
    let (status, html) = send(app, "POST", &format!("/mint/connect/{token}"), None, "application/x-www-form-urlencoded", form.into_bytes(), &[]).await;
    assert_eq!(status, StatusCode::OK, "{html}");
    assert!(html.contains(&format!("https://mint.example/mint/webhooks/seller/scribase/{SELLER}/dodo")));
    assert!(html.contains("https://console.scribase.example/back"));
    token
}

#[tokio::test]
async fn connect_sell_and_forward_entitlements_without_touching_money() {
    let fake = Arc::new(Fake::default());
    let st = state(fake.clone());
    let app = seller::router(st.clone());
    let token = connect_dodo(&app, &fake).await;
    // The link works once.
    let (status, _) = send(&app, "POST", &format!("/mint/connect/{token}"), None, "application/x-www-form-urlencoded", b"api_key=x&webhook_secret=y".to_vec(), &[]).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, seller_view) = api(&app, "GET", &format!("/mint/v1/sellers/{SELLER}"), Some(SCRIBASE_KEY), Value::Null).await;
    assert_eq!(seller_view["connected"], true);
    assert_eq!(seller_view["provider"], "dodo");
    assert!(!seller_view.to_string().contains("seller_dodo_key_123"), "the calling product never sees the key");

    fake.answer("https://test.dodopayments.com/products", 200, json!({"product_id": "pdt_1"}));
    let (status, made) = api(&app, "POST", &format!("/mint/v1/sellers/{SELLER}/products"), Some(SCRIBASE_KEY), json!({"sku":"pro","name":"Pro","price_cents":900,"currency":"USD","interval":"month"})).await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    fake.answer("https://test.dodopayments.com/checkouts", 200, json!({"checkout_url":"https://test.checkout.dodopayments.com/s/1","session_id":"cks_1"}));
    let (status, session) = api(&app, "POST", &format!("/mint/v1/sellers/{SELLER}/checkout-sessions"), Some(SCRIBASE_KEY), json!({"sku":"pro","buyer_ref":"user_1","success_url":"https://shop.example.com/thanks"})).await;
    assert_eq!(status, StatusCode::CREATED, "{session}");
    assert_eq!(session["url"], "https://test.checkout.dodopayments.com/s/1");
    for call in fake.calls_to("https://test.dodopayments.com/") {
        assert!(call.2.contains(&("authorization".to_owned(), "Bearer seller_dodo_key_123".to_owned())), "every processor call uses the seller's own key: {}", call.1);
    }

    // The seller's processor says the buyer paid.
    let meta = json!({"mint_seller": format!("scribase:{SELLER}"), "buyer_ref": "user_1", "sku": "pro"});
    let (raw, headers) = signed_seller_hook(&json!({"type":"subscription.active","data":{"subscription_id":"sub_1","next_billing_date":"2026-10-29T12:00:00Z","metadata":meta}}), "msg_a");
    let hook = format!("/mint/webhooks/seller/scribase/{SELLER}/dodo");
    let (status, text) = send(&app, "POST", &hook, None, "application/json", raw.clone(), &headers).await;
    assert_eq!(status, StatusCode::OK, "{text}");
    let (status, text) = send(&app, "POST", &hook, None, "application/json", raw, &headers).await;
    assert!(status == StatusCode::OK && text.contains("\"recorded\":false"), "a replay records nothing: {text}");
    assert_eq!(seller::deliver_due(&st).await, (1, 0));
    let delivered = fake.calls_to("https://api.scribase.example/");
    assert_eq!(delivered.len(), 1);
    let body: Value = serde_json::from_slice(&delivered[0].3).unwrap();
    assert_eq!(body["type"], "entitlement.granted");
    assert_eq!(body["data"]["buyer_ref"], "user_1");
    assert_eq!(body["data"]["sku"], "pro");
    assert_eq!(body["data"]["expires_at_ms"], 1_793_275_200_000_i64);
    let header = |name: &str| delivered[0].2.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone()).unwrap();
    assert_eq!(sign(PRODUCT_SECRET, &header("webhook-id"), &header("webhook-timestamp"), &delivered[0].3).unwrap(), header("webhook-signature"), "Standard Webhooks signed with the product's secret");

    // A refund on the seller's account revokes it.
    fake.answer("https://test.dodopayments.com/payments/pay_1", 200, json!({"payment_id":"pay_1","metadata":meta}));
    let (raw, headers) = signed_seller_hook(&json!({"type":"refund.succeeded","data":{"payment_id":"pay_1"}}), "msg_b");
    let (status, _) = send(&app, "POST", &hook, None, "application/json", raw, &headers).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(seller::deliver_due(&st).await, (1, 0));
    let last: Value = serde_json::from_slice(&fake.calls_to("https://api.scribase.example/").last().unwrap().3).unwrap();
    assert_eq!((last["type"].as_str(), last["data"]["reason"].as_str()), (Some("entitlement.revoked"), Some("refund")));
    let (_, list) = api(&app, "GET", &format!("/mint/v1/sellers/{SELLER}/entitlements"), Some(SCRIBASE_KEY), Value::Null).await;
    assert_eq!(list["entitlements"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn sellers_are_kept_apart_and_bad_calls_change_nothing() {
    let fake = Arc::new(Fake::default());
    let st = state(fake.clone());
    let app = seller::router(st.clone());
    connect_dodo(&app, &fake).await;
    // Another calling product does not see this seller.
    let (_, view) = api(&app, "GET", &format!("/mint/v1/sellers/{SELLER}"), Some(RALLY_KEY), Value::Null).await;
    assert_eq!(view["connected"], false);
    assert_eq!(api(&app, "POST", &format!("/mint/v1/sellers/{SELLER}/connect"), Some("nope"), Value::Null).await.0, StatusCode::UNAUTHORIZED);
    // A webhook signed with another secret, or naming another seller, records nothing.
    let meta = json!({"mint_seller": "scribase:someone.else", "buyer_ref": "user_1", "sku": "pro"});
    let (raw, headers) = signed_seller_hook(&json!({"type":"payment.succeeded","data":{"payment_id":"pay_9","metadata":meta}}), "msg_c");
    let hook = format!("/mint/webhooks/seller/scribase/{SELLER}/dodo");
    let (status, text) = send(&app, "POST", &hook, None, "application/json", raw.clone(), &headers).await;
    assert!(status == StatusCode::OK && text.contains("ignored"), "{text}");
    let mut forged = headers.clone();
    forged[2].1 = "v1,AAAA".into();
    assert_eq!(send(&app, "POST", &hook, None, "application/json", raw, &forged).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(send(&app, "POST", "/mint/webhooks/seller/scribase/nobody/dodo", None, "application/json", b"{}".to_vec(), &[]).await.0, StatusCode::NOT_FOUND);
    assert_eq!(seller::deliver_due(&st).await, (0, 0));
    // Checkout before a product exists, bad buyer ref, http return: refused.
    assert_eq!(api(&app, "POST", &format!("/mint/v1/sellers/{SELLER}/checkout-sessions"), Some(SCRIBASE_KEY), json!({"sku":"none","buyer_ref":"u","success_url":"https://a.example"})).await.0, StatusCode::NOT_FOUND);
    assert_eq!(api(&app, "POST", &format!("/mint/v1/sellers/{SELLER}/checkout-sessions"), Some(SCRIBASE_KEY), json!({"sku":"pro","buyer_ref":"a b","success_url":"https://a.example"})).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(api(&app, "POST", &format!("/mint/v1/sellers/{SELLER}/products"), Some(SCRIBASE_KEY), json!({"sku":"x","name":"X","price_cents":1,"currency":"USD"})).await.0, StatusCode::BAD_REQUEST);
    // Stripe Connect is off without OD-9: its routes answer not found.
    assert_eq!(send(&app, "POST", "/mint/webhooks/stripe-connect", None, "application/json", b"{}".to_vec(), &[]).await.0, StatusCode::NOT_FOUND);
    // A failed delivery is retried later, not lost.
    let meta = json!({"mint_seller": format!("scribase:{SELLER}"), "buyer_ref": "user_2", "sku": "pro"});
    let (raw, headers) = signed_seller_hook(&json!({"type":"payment.succeeded","data":{"payment_id":"pay_2","metadata":meta}}), "msg_d");
    assert_eq!(send(&app, "POST", &hook, None, "application/json", raw, &headers).await.0, StatusCode::OK);
    fake.answer("https://api.scribase.example/", 500, json!({}));
    assert_eq!(seller::deliver_due(&st).await, (0, 1));
    assert_eq!(seller::deliver_due(&st).await, (0, 0), "backoff: not due again at once");
}

#[tokio::test]
async fn the_mint_ledger_is_never_touched_by_seller_payments() {
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(NOW_MS));
    let state_ledger = AppState::new(
        InMemoryStore::with_clock(clock.clone()),
        clock,
        Arc::new(FakeRamp::new()),
        Arc::new(FakeKycGate::new(KycTier::Basic, i64::MAX)),
        Arc::new(InMemoryPayerKeys::new()),
        Arc::new(FixedSigner::new()),
        "ledger-api-key",
        "whsec_test",
    );
    let fake = Arc::new(Fake::default());
    let st = state(fake.clone());
    // The same router the server serves: ledger routes plus seller routes.
    let app = build_router(state_ledger.clone()).merge(seller::router(st.clone()));
    connect_dodo(&app, &fake).await;
    fake.answer("https://test.dodopayments.com/products", 200, json!({"product_id": "pdt_1"}));
    api(&app, "POST", &format!("/mint/v1/sellers/{SELLER}/products"), Some(SCRIBASE_KEY), json!({"sku":"pro","name":"Pro","price_cents":900,"currency":"USD"})).await;
    let meta = json!({"mint_seller": format!("scribase:{SELLER}"), "buyer_ref": "user_1", "sku": "pro"});
    let (raw, headers) = signed_seller_hook(&json!({"type":"payment.succeeded","data":{"payment_id":"pay_1","total_amount":900,"metadata":meta}}), "msg_e");
    assert_eq!(send(&app, "POST", &format!("/mint/webhooks/seller/scribase/{SELLER}/dodo"), None, "application/json", raw, &headers).await.0, StatusCode::OK);
    seller::deliver_due(&st).await;
    let ledger = state_ledger.ledger.store().inner();
    assert!(ledger.journal_snapshot().is_empty(), "no ledger entry for a seller's sale");
    assert_eq!(ledger.accounts_len(), 0, "no Mint account holds a buyer's money");
    for call in fake.calls.lock().unwrap().iter() {
        assert!(!call.1.contains("/payouts") && !call.1.contains("/refunds") && !call.1.contains("/transfers"), "Mint never moves money: {}", call.1);
    }
}

/// Wave Q, for Menivor QM1.3: a creator's coupon is a discount code made on
/// the creator's own Dodo account through Mint (the key never leaves Mint),
/// limited to the creator's skus, and a checkout can apply it up front.
#[tokio::test]
async fn discount_codes_are_made_on_the_sellers_account_and_reach_checkout() {
    let fake = Arc::new(Fake::default());
    let st = state(fake.clone());
    let app = seller::router(st.clone());
    // Connect the seller (the connect page is the only place a key is typed).
    let (_, link) = api(&app, "POST", &format!("/mint/v1/sellers/{SELLER}/connect"), Some(SCRIBASE_KEY), Value::Null).await;
    let token = link["connect_url"].as_str().unwrap().rsplit('/').next().unwrap().to_owned();
    fake.answer("https://test.dodopayments.com/products?page_size=1", 200, json!({"items": []}));
    let form = format!("mode=test&api_key=seller_dodo_key_123&webhook_secret={}", SELLER_HOOK.replace('=', "%3D"));
    let (status, _) = send(&app, "POST", &format!("/mint/connect/{token}"), None, "application/x-www-form-urlencoded", form.into_bytes(), &[]).await;
    assert_eq!(status, StatusCode::OK);
    fake.answer("https://test.dodopayments.com/products", 200, json!({"product_id": "pdt_title_42"}));
    api(&app, "POST", &format!("/mint/v1/sellers/{SELLER}/products"), Some(SCRIBASE_KEY), json!({"sku":"title.42.buy","name":"Title 42","price_cents":499,"currency":"USD"})).await;

    // A percent code limited to one title, then a flat one (Menivor's body, Dodo's shape).
    fake.answer("https://test.dodopayments.com/discounts", 200, json!({"discount_id":"dsc_1","code":"LAUNCH20","business_id":"bus_1"}));
    let (status, made) = api(
        &app,
        "POST",
        &format!("/mint/v1/sellers/{SELLER}/discounts"),
        Some(SCRIBASE_KEY),
        json!({"code":"launch20","type":"percentage","amount":2000,"expires_at":"2026-12-31T23:59:59Z","name":"Launch","restricted_to":["title.42.buy"]}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{made}");
    assert_eq!(made["discount"]["code"], "LAUNCH20");
    assert_eq!(made["discount"]["id"], "dsc_1");
    let sent: Value = serde_json::from_slice(&fake.calls_to("https://test.dodopayments.com/discounts")[0].3).unwrap();
    assert_eq!(sent["restricted_to"], json!(["pdt_title_42"]), "skus become the seller's own product ids");
    fake.answer("https://test.dodopayments.com/discounts", 200, json!({"discount_id":"dsc_2","code":"FIVEOFF"}));
    let (status, _) = api(
        &app,
        "POST",
        &format!("/mint/v1/sellers/{SELLER}/discounts"),
        Some(SCRIBASE_KEY),
        json!({"code":"FIVEOFF","type":"flat","amount":500,"expires_at":null,"name":"FIVEOFF","currency_options":[{"currency":"usd","max_amount_possible":500,"is_default":true}]}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);
    let sent: Value = serde_json::from_slice(&fake.calls_to("https://test.dodopayments.com/discounts")[1].3).unwrap();
    assert_eq!(sent["currency_options"], json!([{"currency":"USD","max_amount_possible":500,"is_default":true}]));
    for call in fake.calls_to("https://test.dodopayments.com/discounts") {
        assert!(call.2.contains(&("authorization".to_owned(), "Bearer seller_dodo_key_123".to_owned())), "the seller's own key");
    }

    // The checkout applies the code; a malformed code is refused before any call.
    fake.answer("https://test.dodopayments.com/checkouts", 200, json!({"checkout_url":"https://test.checkout.dodopayments.com/s/9","session_id":"cks_9"}));
    let (status, session) = api(&app, "POST", &format!("/mint/v1/sellers/{SELLER}/checkout-sessions"), Some(SCRIBASE_KEY), json!({"sku":"title.42.buy","buyer_ref":"viewer_1","success_url":"https://watch.example/back","discount_code":"launch20"})).await;
    assert_eq!(status, StatusCode::CREATED, "{session}");
    let sent: Value = serde_json::from_slice(&fake.calls_to("https://test.dodopayments.com/checkouts")[0].3).unwrap();
    assert_eq!(sent["discount_codes"], json!(["LAUNCH20"]));
    let before = fake.calls.lock().unwrap().len();
    let (status, _) = api(&app, "POST", &format!("/mint/v1/sellers/{SELLER}/checkout-sessions"), Some(SCRIBASE_KEY), json!({"sku":"title.42.buy","buyer_ref":"viewer_1","success_url":"https://watch.example/back","discount_code":"no spaces!"})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Refused before any processor call: bad type, bad amount, unknown sku, no key, another product's seller.
    for (body, want) in [
        (json!({"code":"X1X","type":"bogo","amount":1}), StatusCode::BAD_REQUEST),
        (json!({"code":"BIG","type":"percentage","amount":10001}), StatusCode::BAD_REQUEST),
        (json!({"code":"NOCUR","type":"flat","amount":100}), StatusCode::BAD_REQUEST),
        (json!({"code":"SKU","type":"percentage","amount":100,"restricted_to":["nope"]}), StatusCode::NOT_FOUND),
    ] {
        assert_eq!(api(&app, "POST", &format!("/mint/v1/sellers/{SELLER}/discounts"), Some(SCRIBASE_KEY), body.clone()).await.0, want, "{body}");
    }
    assert_eq!(api(&app, "POST", &format!("/mint/v1/sellers/{SELLER}/discounts"), None, json!({"code":"ABC","type":"percentage","amount":100})).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(api(&app, "POST", &format!("/mint/v1/sellers/{SELLER}/discounts"), Some(RALLY_KEY), json!({"code":"ABC","type":"percentage","amount":100})).await.0, StatusCode::CONFLICT, "Rally's view of this seller id is not connected");
    assert_eq!(fake.calls.lock().unwrap().len(), before, "no processor call for refused requests");
}
