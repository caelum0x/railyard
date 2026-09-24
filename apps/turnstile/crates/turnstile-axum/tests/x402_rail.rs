//! x402 on the drop-in middleware: one gated axum route speaks BOTH AITP-402
//! and x402. A fake facilitator (behind the real `HttpFacilitator` over the
//! `HttpTransport` seam) answers `/verify` and `/settle`.
//!
//! - unpaid → `402` with the x402 JSON `accepts[]` body AND `AITP-Challenge`;
//! - `X-PAYMENT` → handler runs, `/settle` called, `X-PAYMENT-RESPONSE` set;
//! - handler failure → never settled (payer not charged);
//! - replayed `X-PAYMENT` → `402`, handler not run again;
//! - facilitator rejection → `402` whose `error` carries the reason.

use std::sync::{Arc, Mutex};

use aitp_client::transport::{HttpRequest, HttpResponse, HttpTransport};
use aitp_client::{AmountMicros, PayError};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde_json::{json, Value};
use tower::ServiceExt;
use turnstile_axum::admit;
use turnstile_core::x402::{
    HttpFacilitator, InMemoryX402Journal, X402Config, X402Gate, X402Journal, X402Phase,
};
use turnstile_core::{
    ChallengeIssuer, FakeVerifier, InMemoryNonceStore, Turnstile, TurnstileConfig,
};

const PAY_TO: &str = "0x2222222222222222222222222222222222222222";
const PAYER: &str = "0x1111111111111111111111111111111111111111";

struct FakeFacilitator {
    valid: bool,
    calls: Mutex<Vec<String>>,
}

#[async_trait]
impl HttpTransport for FakeFacilitator {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, PayError> {
        self.calls.lock().unwrap().push(req.path.clone());
        let sent: Value = serde_json::from_slice(&req.body).unwrap();
        assert_eq!(sent["x402Version"], 1);
        assert_eq!(sent["paymentRequirements"]["payTo"], PAY_TO);
        let body = match req.path.as_str() {
            "/verify" if self.valid => json!({ "isValid": true, "payer": PAYER }),
            "/verify" => json!({ "isValid": false, "invalidReason": "insufficient_funds" }),
            "/settle" => json!({
                "success": true, "transaction": "0xfeed", "network": "base-sepolia", "payer": PAYER
            }),
            other => panic!("unexpected facilitator path {other}"),
        };
        Ok(HttpResponse {
            status: if self.valid { 200 } else { 400 },
            headers: Vec::new(),
            body: body.to_string().into_bytes(),
        })
    }
}

fn x402_config() -> X402Config {
    X402Config {
        pay_to: PAY_TO.into(),
        network: "base-sepolia".into(),
        facilitator_url: "https://facilitator.test".into(),
        facilitator_auth: None,
        asset: turnstile_core::x402::config::USDC_BASE_SEPOLIA.into(),
        asset_name: "USD Coin".into(),
        asset_version: "2".into(),
        max_timeout_seconds: 60,
        mint_account: None,
        public_url: None,
    }
}

struct Harness {
    app: Router,
    facilitator: Arc<FakeFacilitator>,
    journal: Arc<InMemoryX402Journal>,
}

fn harness(valid: bool, handler_status: StatusCode) -> Harness {
    let facilitator = Arc::new(FakeFacilitator {
        valid,
        calls: Mutex::new(Vec::new()),
    });
    let journal = Arc::new(InMemoryX402Journal::new());
    let x402 = Arc::new(X402Gate::new(
        x402_config(),
        Arc::new(HttpFacilitator::new(facilitator.clone(), None)),
        journal.clone(),
        None,
    ));
    let config = TurnstileConfig {
        mint_server_url: "http://mint.internal".into(),
        api_key: "k".into(),
        payee: "ait1qpayee".into(),
        price_micros: AmountMicros(5_000),
        currency: "AIT".into(),
        challenge_ttl_ms: 60_000,
    };
    let nonces = Arc::new(InMemoryNonceStore::new());
    let gate = Arc::new(
        Turnstile::new(
            ChallengeIssuer::new(config, nonces.clone()),
            Arc::new(FakeVerifier::with_hold("hold_1")),
            nonces,
        )
        .with_x402(x402, "Echo tool"),
    );
    let app = Router::new().route(
        "/v1/echo",
        post(move || async move { (handler_status, "paid body").into_response() })
            .route_layer(from_fn_with_state(gate, admit)),
    );
    Harness {
        app,
        facilitator,
        journal,
    }
}

fn payment(nonce: &str) -> String {
    let payload = json!({
        "x402Version": 1,
        "scheme": "exact",
        "network": "base-sepolia",
        "payload": {
            "signature": "0xsig",
            "authorization": {
                "from": PAYER, "to": PAY_TO, "value": "5000",
                "validAfter": "0", "validBefore": "99999999999", "nonce": nonce
            }
        }
    });
    STANDARD.encode(payload.to_string())
}

async fn call(app: &Router, x_payment: Option<&str>) -> axum::response::Response {
    let mut builder = Request::post("/v1/echo");
    if let Some(p) = x_payment {
        builder = builder.header("x-payment", p);
    }
    app.clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn json_body(resp: axum::response::Response) -> Value {
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn unpaid_request_advertises_both_x402_and_aitp() {
    let h = harness(true, StatusCode::OK);
    let resp = call(&h.app, None).await;
    assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);
    assert!(resp.headers().contains_key("aitp-challenge"));
    let body = json_body(resp).await;
    assert_eq!(body["x402Version"], 1);
    let accept = &body["accepts"][0];
    assert_eq!(accept["scheme"], "exact");
    assert_eq!(accept["maxAmountRequired"], "5000");
    assert_eq!(accept["payTo"], PAY_TO);
    assert_eq!(accept["resource"], "/v1/echo");
    assert_eq!(accept["description"], "Echo tool");
}

#[tokio::test]
async fn x_payment_is_verified_settled_and_answered_with_payment_response() {
    let h = harness(true, StatusCode::OK);
    let resp = call(&h.app, Some(&payment("0xaa"))).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let header = resp
        .headers()
        .get("x-payment-response")
        .unwrap()
        .to_str()
        .unwrap();
    let settled: Value = serde_json::from_slice(&STANDARD.decode(header).unwrap()).unwrap();
    assert_eq!(settled["success"], true);
    assert_eq!(settled["transaction"], "0xfeed");
    assert_eq!(
        *h.facilitator.calls.lock().unwrap(),
        vec!["/verify", "/settle"]
    );
    assert_eq!(
        h.journal.recent(1).await.unwrap()[0].phase,
        X402Phase::Settled
    );

    // The same signed authorization cannot buy a second call.
    let replay = call(&h.app, Some(&payment("0xaa"))).await;
    assert_eq!(replay.status(), StatusCode::PAYMENT_REQUIRED);
    let body = json_body(replay).await;
    assert_eq!(
        body["error"],
        "invalid_exact_evm_payload_authorization_nonce_used"
    );
    assert_eq!(h.facilitator.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn handler_failure_is_never_settled() {
    let h = harness(true, StatusCode::INTERNAL_SERVER_ERROR);
    let resp = call(&h.app, Some(&payment("0xbb"))).await;
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(resp.headers().get("x-payment-response").is_none());
    assert_eq!(*h.facilitator.calls.lock().unwrap(), vec!["/verify"]);
    assert_eq!(
        h.journal.recent(1).await.unwrap()[0].phase,
        X402Phase::Abandoned
    );
}

#[tokio::test]
async fn facilitator_rejection_is_a_402_with_the_reason() {
    let h = harness(false, StatusCode::OK);
    let resp = call(&h.app, Some(&payment("0xcc"))).await;
    assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);
    assert!(resp.headers().contains_key("aitp-challenge"));
    let body = json_body(resp).await;
    assert_eq!(body["error"], "insufficient_funds");
    assert_eq!(body["accepts"][0]["payTo"], PAY_TO);
}
