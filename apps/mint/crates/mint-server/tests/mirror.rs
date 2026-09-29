//! Wave P PY1.5: the portfolio mirror records each product's paid events once,
//! keeps products apart, delivers signed outbound webhooks with retries, and
//! never touches the ledger.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use mint_payments::{HttpResponse, HttpTransport, TransportError};
use mint_server::mirror::{self, Endpoint, MirrorKey, MirrorState};
use mint_store::{MemoryMirror, MirrorStore};
use serde_json::{json, Value};
use tower::ServiceExt;

const SCRIBASE_KEY: &str = "scribase-mirror-key-0123456789";
const RALLY_KEY: &str = "rally-mirror-key-0123456789abcd";
const SECRET: &str = "whsec_Y29udHJhY3QtZml4dHVyZS1zZWNyZXQtMzItYnl0ZXM=";

type Call = (String, Vec<(String, String)>, Vec<u8>);

#[derive(Default)]
struct FakeTransport {
    calls: Mutex<Vec<Call>>,
    statuses: Mutex<Vec<u16>>,
}

#[async_trait]
impl HttpTransport for FakeTransport {
    async fn get(&self, _url: &str, _headers: &[(String, String)]) -> Result<HttpResponse, TransportError> {
        Err(TransportError("not used".into()))
    }
    async fn post_json(&self, url: &str, headers: &[(String, String)], body: &[u8]) -> Result<HttpResponse, TransportError> {
        self.calls.lock().unwrap().push((url.to_owned(), headers.to_vec(), body.to_vec()));
        let status = self.statuses.lock().unwrap().pop().unwrap_or(200);
        Ok(HttpResponse { status, body: Vec::new() })
    }
}

fn now() -> i64 {
    1_790_683_200_000
}

fn state(store: Arc<MemoryMirror>, transport: Arc<FakeTransport>) -> MirrorState {
    MirrorState {
        store,
        keys: Arc::new(vec![
            MirrorKey { product: "scribase".into(), key: SCRIBASE_KEY.into() },
            MirrorKey { product: "rally".into(), key: RALLY_KEY.into() },
        ]),
        endpoints: Arc::new(vec![Endpoint {
            name: "scribase-billing".into(),
            url: "https://scribase.example/mint/events".into(),
            secret: SECRET.into(),
            products: vec!["scribase".into()],
        }]),
        transport,
        now_ms: now,
    }
}

async fn call(st: &MirrorState, method: &str, key: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
    let mut req = Request::builder().method(method).uri(if method == "GET" { "/v1/mirror/events?limit=10" } else { "/v1/mirror/events" });
    if let Some(k) = key {
        req = req.header("x-api-key", k);
    }
    let req = match body {
        Some(b) => req.header("content-type", "application/json").body(Body::from(b.to_string())).unwrap(),
        None => req.body(Body::empty()).unwrap(),
    };
    let res = mirror::router(st.clone()).oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

fn paid(event_id: &str) -> Value {
    json!({ "provider": "dodo", "provider_event_id": event_id, "kind": "payment", "amount_micros": 10_000_000,
            "currency": "USD", "customer_ref": "org_acme", "occurred_at_ms": 1_790_683_100_000_i64 })
}

#[tokio::test]
async fn a_paid_event_is_recorded_once_and_products_never_see_each_other() {
    let store = Arc::new(MemoryMirror::default());
    let st = state(store.clone(), Arc::new(FakeTransport::default()));
    let (s1, b1) = call(&st, "POST", Some(SCRIBASE_KEY), Some(paid("pay_1"))).await;
    assert_eq!(s1, StatusCode::CREATED, "{b1}");
    assert_eq!(b1["event"]["product"], "scribase");
    let (s2, b2) = call(&st, "POST", Some(SCRIBASE_KEY), Some(paid("pay_1"))).await;
    assert_eq!(s2, StatusCode::OK);
    assert_eq!(b2["duplicate"], true);
    let (_, list) = call(&st, "GET", Some(SCRIBASE_KEY), None).await;
    assert_eq!(list["events"].as_array().unwrap().len(), 1);
    let (_, rally) = call(&st, "GET", Some(RALLY_KEY), None).await;
    assert_eq!(rally["events"].as_array().unwrap().len(), 0, "rally cannot read scribase's records");
    let (s3, _) = call(&st, "POST", None, Some(paid("pay_2"))).await;
    assert_eq!(s3, StatusCode::UNAUTHORIZED);
    let (s4, _) = call(&st, "POST", Some("wrong-key-000000000000000000"), Some(paid("pay_2"))).await;
    assert_eq!(s4, StatusCode::UNAUTHORIZED);
    let mut bad = paid("pay_3");
    bad["kind"] = json!("payout");
    assert_eq!(call(&st, "POST", Some(SCRIBASE_KEY), Some(bad)).await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn deliveries_are_signed_and_retried_until_a_2xx() {
    let store = Arc::new(MemoryMirror::default());
    let transport = Arc::new(FakeTransport::default());
    // First attempt fails with 500, second succeeds.
    *transport.statuses.lock().unwrap() = vec![200, 500];
    let st = state(store.clone(), transport.clone());
    let (_, b) = call(&st, "POST", Some(SCRIBASE_KEY), Some(paid("pay_9"))).await;
    let id = b["event"]["id"].as_str().unwrap().to_owned();
    assert_eq!(mirror::deliver_due(&st).await, (0, 1));
    assert_eq!(store.delivered_at(&id, "scribase-billing"), None);
    // Not due again until the backoff passes.
    assert_eq!(store.due(now(), 10).await.unwrap().len(), 0);
    let later = MirrorState { now_ms: || 1_790_683_200_000 + 31_000, ..st.clone() };
    assert_eq!(mirror::deliver_due(&later).await, (1, 0));
    assert!(store.delivered_at(&id, "scribase-billing").is_some());
    let second = transport.calls.lock().unwrap()[1].clone();
    let (url, headers, body) = &second;
    assert_eq!(url, "https://scribase.example/mint/events");
    let header = |n: &str| headers.iter().find(|(k, _)| k == n).map(|(_, v)| v.clone()).unwrap();
    let expected = mint_payments::outbound::sign(SECRET, &header("webhook-id"), &header("webhook-timestamp"), body).unwrap();
    assert_eq!(header("webhook-signature"), expected);
    let parsed: Value = serde_json::from_slice(body).unwrap();
    assert_eq!(parsed["type"], "mirror.recorded");
    assert_eq!(parsed["data"]["provider_event_id"], "pay_9");
    // A rally record is not sent to the scribase-only endpoint.
    call(&st, "POST", Some(RALLY_KEY), Some(paid("pay_r1"))).await;
    assert_eq!(store.due(i64::MAX, 10).await.unwrap().len(), 0);
}

#[test]
fn configuration_is_checked_and_backoff_is_bounded() {
    assert!(mirror::parse_keys(r#"[{"product":"scribase","key":"short"}]"#).is_err());
    assert!(mirror::parse_endpoints(r#"[{"name":"x","url":"http://evil.example","secret":"whsec_AA=="}]"#).is_err());
    assert!(mirror::parse_endpoints(r#"[{"name":"x","url":"https://a.example","secret":"plain"}]"#).is_err());
    assert_eq!(mirror::backoff_ms(0), 30_000);
    assert_eq!(mirror::backoff_ms(20), 6 * 3_600_000);
}
