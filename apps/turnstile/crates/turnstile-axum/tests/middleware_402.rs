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

use aitp_client::codec::{decode_challenge, encode_authorization};
use aitp_client::transport::{HttpRequest, HttpResponse, HttpTransport};
use aitp_client::{AmountMicros, PayError, PaymentAuthorization, PaymentChallenge};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use tower::ServiceExt; // for `oneshot`
use turnstile_axum::admit;
use turnstile_core::{
    ChallengeIssuer, InMemoryNonceStore, MintVerifier, NonceStore, Turnstile, TurnstileConfig,
};

const RESOURCE: &str = "POST /v1/echo";

// --- fake mint-server -------------------------------------------------------

struct FakeMintTransport {
    recorded: Mutex<Vec<HttpRequest>>,
}

impl FakeMintTransport {
    fn new() -> Self {
        Self {
            recorded: Mutex::new(Vec::new()),
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
        let body = if path == "/v1/aitp/verify" {
            r#"{"success":true,"data":{"scheme":"hold","hold_id":"h1"}}"#
        } else if path.starts_with("/v1/addresses/") {
            r#"{"success":true,"data":{"account_id":"acct_payee"}}"#
        } else if path.ends_with("/capture") {
            r#"{"success":true,"data":{"first_seq":1,"captured_micros":5,"holder_balance":{"available_micros":100}}}"#
        } else {
            r#"{"success":true,"data":{}}"#
        };
        Ok(HttpResponse::new(200, body.as_bytes().to_vec()))
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
    let challenge = gate.challenge(RESOURCE);
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

    let challenge = gate.challenge(RESOURCE);
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

    let challenge = gate.challenge(RESOURCE);
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
    assert!(nonces.consume(&challenge.nonce, 0).is_err());
}
