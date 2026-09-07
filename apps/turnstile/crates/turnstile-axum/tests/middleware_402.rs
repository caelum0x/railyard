//! Integration tests for the drop-in `admit` middleware on a real axum Router.
//!
//! A `FakeMintTransport` implementing aitp-client's `HttpTransport` seam stands
//! in for a live mint-server (records requests, replays canned responses). We
//! assert the full edge behaviour:
//! - unpaid → `402` with `AITP-Challenge` + `AITP-Version`, inner handler NOT run;
//! - valid `AITP-Authorization` → `200`, handler body, `AITP-Receipt` attached,
//!   and a capture recorded on the mint;
//! - handler `500` → the hold is released, never captured;
//! - replayed authorization → `402` (single-use nonce guard).

use std::sync::{Arc, Mutex};

use aitp_client::codec::{decode_challenge, encode_authorization, encode_receipt};
use aitp_client::transport::{HttpRequest, HttpResponse, HttpTransport};
use aitp_client::{AmountMicros, PayError, PaymentAuthorization, PaymentChallenge, Receipt};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use tower::ServiceExt; // for `oneshot`
use turnstile_axum::{admit, PaidRequest};
use turnstile_core::{
    ChallengeIssuer, InMemoryNonceStore, MintVerifier, NonceStore, Turnstile, TurnstileConfig,
    TurnstileError,
};

const RESOURCE: &str = "POST /v1/echo";

// --- fake mint-server -------------------------------------------------------

struct FakeMintTransport {
    recorded: Mutex<Vec<HttpRequest>>,
    fail_capture: bool,
    fail_release: bool,
}

impl FakeMintTransport {
    fn new() -> Self {
        Self {
            recorded: Mutex::new(Vec::new()),
            fail_capture: false,
            fail_release: false,
        }
    }
    fn failing_capture() -> Self {
        Self {
            recorded: Mutex::new(Vec::new()),
            fail_capture: true,
            fail_release: false,
        }
    }
    fn failing_release() -> Self {
        Self {
            recorded: Mutex::new(Vec::new()),
            fail_capture: false,
            fail_release: true,
        }
    }
    fn paths(&self) -> Vec<String> {
        self.recorded
            .lock()
            .unwrap()
            .iter()
            .map(|r| r.path.clone())
            .collect()
    }
}

#[async_trait]
impl HttpTransport for FakeMintTransport {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, PayError> {
        self.recorded.lock().unwrap().push(req.clone());
        let path = req.path.as_str();
        if (self.fail_capture && path.ends_with("/capture"))
            || (self.fail_release && path.ends_with("/release"))
        {
            return Ok(HttpResponse::new(
                503,
                br#"{"error":{"code":"mint_unavailable","message":"retry"}}"#.to_vec(),
            ));
        }
        let body = if path == "/v1/aitp/verify" {
            r#"{"success":true,"data":{"scheme":"hold","hold_id":"h1"}}"#
        } else if path.starts_with("/v1/addresses/") {
            r#"{"success":true,"data":{"account_id":"acct_payee"}}"#
        } else if path.ends_with("/capture") {
            r#"{"success":true,"data":{"first_seq":1,"captured_micros":5,"holder_balance":{"available_micros":100}}}"#
        } else {
            r#"{"success":true,"data":{}}"#
        };
        let response = HttpResponse::new(200, body.as_bytes().to_vec());
        if path.ends_with("/capture") {
            let receipt = Receipt {
                seq: 1,
                op: "hold.capture".to_string(),
                op_hash: "11".repeat(32),
                amount_micros: AmountMicros(1_000),
                balance_after_micros: AmountMicros(100),
                prev_hash: "22".repeat(32),
                hash: "33".repeat(32),
                sig: "44".repeat(64),
            };
            Ok(response.with_header("aitp-receipt", encode_receipt(&receipt)?))
        } else {
            Ok(response)
        }
    }
}

// --- wiring -----------------------------------------------------------------

fn config() -> TurnstileConfig {
    TurnstileConfig {
        mint_server_url: "http://mint.internal".into(),
        api_key: "k".into(),
        payee: "ait1qpayee".into(),
        price_micros: AmountMicros(1_000),
        currency: "AIT".into(),
        challenge_ttl_ms: 60_000,
    }
}

/// Build a gate sharing `nonces` and `transport` so tests can inspect them.
fn gate(nonces: Arc<InMemoryNonceStore>, transport: Arc<FakeMintTransport>) -> Arc<Turnstile> {
    let verifier = Arc::new(MintVerifier::new(transport, "k", "ait1qpayee"));
    let issuer = ChallengeIssuer::new(config(), nonces.clone());
    Arc::new(Turnstile::new(issuer, verifier, nonces))
}

async fn ok_handler(body: String) -> impl IntoResponse {
    (StatusCode::OK, body)
}

async fn failing_handler() -> impl IntoResponse {
    (StatusCode::INTERNAL_SERVER_ERROR, "boom\n")
}

/// A handler that misbehaves by reporting an actual charge ABOVE the quoted
/// price. The extractor must clamp it back to the quote so capture stays within
/// the reserved hold.
async fn over_reporting_handler(paid: PaidRequest) -> impl IntoResponse {
    paid.report_actual(AmountMicros(5_000)); // quote is 1_000
    (StatusCode::OK, "done")
}

/// A [`NonceStore`] whose every operation fails, standing in for a durable store
/// (e.g. Postgres) that is unreachable. Used to drive the fail-closed branch
/// where a rejected authorization cannot even mint a fresh challenge.
struct FailingNonceStore;

#[async_trait]
impl NonceStore for FailingNonceStore {
    async fn register(&self, _challenge: &PaymentChallenge) -> Result<(), TurnstileError> {
        Err(TurnstileError::Storage("nonce store unavailable".into()))
    }
    async fn get(&self, _nonce: &str) -> Result<Option<PaymentChallenge>, TurnstileError> {
        Err(TurnstileError::Storage("nonce store unavailable".into()))
    }
    async fn consume(
        &self,
        _nonce: &str,
        _now_ms: i64,
    ) -> Result<PaymentChallenge, TurnstileError> {
        Err(TurnstileError::Storage("nonce store unavailable".into()))
    }
    async fn sweep(&self, _now_ms: i64) -> Result<u64, TurnstileError> {
        Err(TurnstileError::Storage("nonce store unavailable".into()))
    }
}

/// An authorization echoing a challenge; the fake mint accepts it regardless of
/// signature (crypto verification is the real mint's job, faked at the seam).
fn authorization_for(ch: &PaymentChallenge) -> PaymentAuthorization {
    PaymentAuthorization {
        payer: "acct_payer".into(),
        payee: ch.payee.clone(),
        amount_micros: ch.amount_micros,
        nonce: ch.nonce.clone(),
        resource: ch.resource.clone(),
        expiry: ch.expiry,
        scheme: ch.scheme,
        idempotency_key: format!("aitp:{}", ch.nonce),
        pubkey_id: "key_1".into(),
        op_hash: String::new(),
        sig: String::new(),
    }
}

// --- tests ------------------------------------------------------------------

#[tokio::test]
async fn unpaid_request_gets_402_and_does_not_run_handler() {
    let nonces = Arc::new(InMemoryNonceStore::new());
    let transport = Arc::new(FakeMintTransport::new());
    let gate = gate(nonces, transport.clone());

    let app = Router::new().route(
        "/v1/echo",
        post(ok_handler).route_layer(from_fn_with_state(gate.clone(), admit)),
    );

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/echo")
                .body(Body::from("hello"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);
    let challenge_hdr = resp
        .headers()
        .get("aitp-challenge")
        .expect("AITP-Challenge header present");
    assert_eq!(resp.headers().get("aitp-version").unwrap(), "0.1");

    // The challenge is a well-formed, decodable AITP-402 challenge.
    let ch = decode_challenge(challenge_hdr.to_str().unwrap()).unwrap();
    assert_eq!(resp.headers()["aitp-request-id"], ch.nonce);
    assert_eq!(ch.resource, RESOURCE);
    assert_eq!(ch.payee, "ait1qpayee");
    assert_eq!(ch.amount_micros, AmountMicros(1_000));

    // No mint round-trip on an unpaid request.
    assert!(transport.paths().is_empty());
}

#[tokio::test]
async fn valid_authorization_admits_captures_and_attaches_receipt() {
    let nonces = Arc::new(InMemoryNonceStore::new());
    let transport = Arc::new(FakeMintTransport::new());
    let gate = gate(nonces, transport.clone());

    let app = Router::new().route(
        "/v1/echo",
        post(ok_handler).route_layer(from_fn_with_state(gate.clone(), admit)),
    );

    // Issue a real challenge (registers the nonce) as the 402 would.
    let challenge = gate.challenge(RESOURCE).await.unwrap();
    let header = encode_authorization(&authorization_for(&challenge)).unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/echo")
                .header("aitp-authorization", header)
                .body(Body::from("hello"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert!(
        resp.headers().get("aitp-receipt").is_some(),
        "AITP-Receipt attached on capture"
    );
    assert_eq!(resp.headers()["aitp-request-id"], challenge.nonce);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], b"hello");

    // The mint saw verify → address-resolve → capture (success-only).
    let paths = transport.paths();
    assert!(paths.contains(&"/v1/aitp/verify".to_string()));
    assert!(paths.iter().any(|p| p == "/v1/holds/h1/capture"));
    assert!(
        !paths.iter().any(|p| p.ends_with("/release")),
        "success path must not release"
    );
}

#[tokio::test]
async fn handler_error_releases_hold_without_capture() {
    let nonces = Arc::new(InMemoryNonceStore::new());
    let transport = Arc::new(FakeMintTransport::new());
    let gate = gate(nonces, transport.clone());

    let app = Router::new().route(
        "/v1/echo",
        post(failing_handler).route_layer(from_fn_with_state(gate.clone(), admit)),
    );

    let challenge = gate.challenge(RESOURCE).await.unwrap();
    let header = encode_authorization(&authorization_for(&challenge)).unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/echo")
                .header("aitp-authorization", header)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let paths = transport.paths();
    assert!(
        paths.iter().any(|p| p == "/v1/holds/h1/release"),
        "handler failure releases the hold; paths were {paths:?}"
    );
    assert!(
        !paths.iter().any(|p| p.ends_with("/capture")),
        "handler failure must never capture"
    );
}

#[tokio::test]
async fn replayed_authorization_is_rejected_with_402() {
    let nonces = Arc::new(InMemoryNonceStore::new());
    let transport = Arc::new(FakeMintTransport::new());
    let gate = gate(nonces.clone(), transport.clone());

    let app = Router::new().route(
        "/v1/echo",
        post(ok_handler).route_layer(from_fn_with_state(gate.clone(), admit)),
    );

    let challenge = gate.challenge(RESOURCE).await.unwrap();
    let header = encode_authorization(&authorization_for(&challenge)).unwrap();

    let make = || {
        Request::builder()
            .method("POST")
            .uri("/v1/echo")
            .header("aitp-authorization", header.clone())
            .body(Body::from("hi"))
            .unwrap()
    };

    let first = app.clone().oneshot(make()).await.unwrap();
    assert_eq!(first.status(), StatusCode::OK);

    // Same nonce again → single-use replay guard rejects → re-challenge 402.
    let second = app.clone().oneshot(make()).await.unwrap();
    assert_eq!(second.status(), StatusCode::PAYMENT_REQUIRED);

    // The nonce store swept the consumed nonce (it is gone, not merely flagged).
    assert!(nonces.consume(&challenge.nonce, 0).await.is_err());
}

#[tokio::test]
async fn successful_handler_fails_closed_when_capture_is_pending() {
    let nonces = Arc::new(InMemoryNonceStore::new());
    let transport = Arc::new(FakeMintTransport::failing_capture());
    let gate = gate(nonces, transport.clone());
    let app = Router::new().route(
        "/v1/echo",
        post(ok_handler).route_layer(from_fn_with_state(gate.clone(), admit)),
    );
    let challenge = gate.challenge(RESOURCE).await.unwrap();
    let header = encode_authorization(&authorization_for(&challenge)).unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/echo")
                .header("aitp-authorization", header)
                .body(Body::from("work"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()["aitp-settlement"], "capture_pending");
    assert_eq!(response.headers()["aitp-request-id"], challenge.nonce);
    assert!(response.headers().get("aitp-receipt").is_none());
    assert!(transport
        .paths()
        .iter()
        .any(|path| path.ends_with("/capture")));
    assert!(!transport
        .paths()
        .iter()
        .any(|path| path.ends_with("/release")));
}

#[tokio::test]
async fn handler_error_marks_release_pending_when_mint_is_unavailable() {
    let nonces = Arc::new(InMemoryNonceStore::new());
    let transport = Arc::new(FakeMintTransport::failing_release());
    let gate = gate(nonces, transport);
    let app = Router::new().route(
        "/v1/echo",
        post(failing_handler).route_layer(from_fn_with_state(gate.clone(), admit)),
    );
    let challenge = gate.challenge(RESOURCE).await.unwrap();
    let header = encode_authorization(&authorization_for(&challenge)).unwrap();

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/echo")
                .header("aitp-authorization", header)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(response.headers()["aitp-settlement"], "release_pending");
    assert_eq!(response.headers()["aitp-request-id"], challenge.nonce);
    assert!(response.headers().get("aitp-error").is_some());
}

#[tokio::test]
async fn rejected_authorization_fails_closed_when_no_fresh_challenge_can_be_minted() {
    // A malformed authorization is rejected outright, and the durable nonce store
    // is down, so no fresh challenge can be minted. The middleware must still fail
    // closed with a 402 that carries the reason but NO challenge to solve.
    let transport = Arc::new(FakeMintTransport::new());
    let verifier = Arc::new(MintVerifier::new(transport, "k", "ait1qpayee"));
    let nonces: Arc<dyn NonceStore> = Arc::new(FailingNonceStore);
    let issuer = ChallengeIssuer::new(config(), nonces.clone());
    let gate = Arc::new(Turnstile::new(issuer, verifier, nonces));

    let app = Router::new().route(
        "/v1/echo",
        post(ok_handler).route_layer(from_fn_with_state(gate, admit)),
    );

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/echo")
                .header("aitp-authorization", "not-base64url!!")
                .body(Body::from("hello"))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::PAYMENT_REQUIRED);
    assert_eq!(resp.headers()["aitp-version"], "0.1");
    assert!(
        resp.headers().get("aitp-error").is_some(),
        "fail-closed 402 surfaces the rejection reason"
    );
    assert!(
        resp.headers().get("aitp-challenge").is_none(),
        "no fresh challenge can be minted, so none is offered (client cannot proceed unpaid)"
    );
}

#[tokio::test]
async fn over_reported_actual_is_clamped_to_the_quoted_price() {
    let nonces = Arc::new(InMemoryNonceStore::new());
    let transport = Arc::new(FakeMintTransport::new());
    let gate = gate(nonces, transport.clone());

    let app = Router::new().route(
        "/v1/echo",
        post(over_reporting_handler).route_layer(from_fn_with_state(gate.clone(), admit)),
    );

    let challenge = gate.challenge(RESOURCE).await.unwrap();
    let header = encode_authorization(&authorization_for(&challenge)).unwrap();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/echo")
                .header("aitp-authorization", header)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Without clamping, capturing 5_000 against a 1_000 hold would error and the
    // request would 503 with capture_pending. Clamping keeps it a clean success.
    assert_eq!(resp.status(), StatusCode::OK);
    assert!(resp.headers().get("aitp-receipt").is_some());

    // The capture the mint actually saw was for the quoted 1_000, not 5_000.
    let recorded = transport.recorded.lock().unwrap();
    let capture = recorded
        .iter()
        .find(|r| r.path.ends_with("/capture"))
        .expect("a capture was sent");
    let body: serde_json::Value = serde_json::from_slice(&capture.body).unwrap();
    assert_eq!(body["amount_micros"], 1_000);
}
