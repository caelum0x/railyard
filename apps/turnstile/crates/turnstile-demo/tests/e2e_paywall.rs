//! End-to-end paywall loop: the AGENT side uses the REAL aitp-client (blake3
//! `op_hash` + `Ed25519Signer` + `codec::encode_authorization`); the MINT side is
//! faked ONLY at aitp-client's `HttpTransport` seam (a `FakeMintTransport` that
//! records requests and replays canned responses). No live mint-server.
//!
//! This proves the <10-line gating and the full path:
//!   unpaid → `402` + `AITP-Challenge`
//!   → agent signs the pinned intent and retries
//!   → verify → admit → capture (success-only) + `AITP-Receipt`
//!   → replay the same authorization → rejected.

use std::sync::{Arc, Mutex};

use aitp_client::codec::{decode_challenge, encode_authorization};
use aitp_client::signer::{op_hash, op_hash_hex, AuthSigner, AuthorizationIntent, Ed25519Signer};
use aitp_client::transport::{HttpRequest, HttpResponse, HttpTransport};
use aitp_client::{AmountMicros, PayError, PaymentAuthorization, PaymentChallenge};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use tower::ServiceExt;
use turnstile_axum::{admit, PaidRequest};
use turnstile_core::{
    ChallengeIssuer, InMemoryNonceStore, MintVerifier, Turnstile, TurnstileConfig,
};

const RESOURCE: &str = "POST /v1/echo";

// --- fake mint-server at the aitp-client transport seam --------------------

struct FakeMintTransport {
    recorded: Mutex<Vec<HttpRequest>>,
}

impl FakeMintTransport {
    fn new() -> Self {
        Self {
            recorded: Mutex::new(Vec::new()),
        }
    }
    fn recorded(&self) -> Vec<HttpRequest> {
        self.recorded.lock().unwrap().clone()
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

// --- the turnstile server under test ---------------------------------------

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

async fn echo(paid: PaidRequest, body: String) -> impl IntoResponse {
    let quoted = paid.price().get();
    let used = (body.len() as i64).clamp(0, quoted);
    paid.report_actual(AmountMicros::new(used));
    (StatusCode::OK, body)
}

fn server(transport: Arc<FakeMintTransport>) -> Router {
    let verifier = Arc::new(MintVerifier::new(transport, "k", "ait1qpayee"));
    let nonces = Arc::new(InMemoryNonceStore::new());
    let issuer = ChallengeIssuer::new(config(), nonces.clone());
    let gate = Arc::new(Turnstile::new(issuer, verifier, nonces));

    // The whole point: gate a route in a few lines.
    Router::new().route(
        "/v1/echo",
        post(echo).route_layer(from_fn_with_state(gate.clone(), admit)),
    )
}

// --- REAL agent-side signing (mirrors aitp-client MintPaymentGateway) -------

fn sign_authorization(
    signer: &Ed25519Signer,
    payer: &str,
    ch: &PaymentChallenge,
) -> PaymentAuthorization {
    let idempotency_key = format!("aitp:{}", ch.nonce);
    let intent = AuthorizationIntent {
        payer,
        payee: &ch.payee,
        amount_micros: ch.amount_micros,
        nonce: &ch.nonce,
        resource: &ch.resource,
        expiry: ch.expiry,
        idempotency_key: &idempotency_key,
    };
    let hash = op_hash(&intent);
    PaymentAuthorization {
        payer: payer.to_string(),
        payee: ch.payee.clone(),
        amount_micros: ch.amount_micros,
        nonce: ch.nonce.clone(),
        resource: ch.resource.clone(),
        expiry: ch.expiry,
        idempotency_key,
        pubkey_id: signer.pubkey_id(),
        op_hash: op_hash_hex(&hash),
        sig: signer.sign_op_hash(&hash),
    }
}

// --- the test ---------------------------------------------------------------

#[tokio::test]
async fn full_402_authorize_verify_capture_then_replay_rejected() {
    let transport = Arc::new(FakeMintTransport::new());
    let app = server(transport.clone());

    // (1) UNPAID → 402 CHALLENGE.
    let unpaid = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/echo")
                .body(Body::from("hello world"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(unpaid.status(), StatusCode::PAYMENT_REQUIRED);
    let challenge = decode_challenge(
        unpaid
            .headers()
            .get("aitp-challenge")
            .unwrap()
            .to_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(challenge.resource, RESOURCE);

    // (2) AGENT AUTHORIZES — REAL blake3 op_hash + ed25519 signature.
    let signer = Ed25519Signer::from_seed("key_agent", &[9u8; 32]);
    let auth = sign_authorization(&signer, "acct_payer", &challenge);
    assert_eq!(auth.op_hash.len(), 64, "real 32-byte blake3 hex op_hash");
    assert!(!auth.sig.is_empty(), "real ed25519 signature present");
    let header = encode_authorization(&auth).unwrap();

    // (3)+(4)+(5)+(6a) RETRY → verify → admit → run handler → capture.
    let paid = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/echo")
                .header("aitp-authorization", header.clone())
                .body(Body::from("hello world"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(paid.status(), StatusCode::OK);
    assert!(
        paid.headers().get("aitp-receipt").is_some(),
        "AITP-Receipt attached after capture"
    );
    let body = axum::body::to_bytes(paid.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], b"hello world");

    // The mint saw the agent's already-signed authorization forwarded verbatim,
    // then a capture of the ACTUAL usage (11 bytes) — success-only settlement.
    let rec = transport.recorded();
    let verify = rec
        .iter()
        .find(|r| r.path == "/v1/aitp/verify")
        .expect("verify forwarded");
    let forwarded: PaymentAuthorization = serde_json::from_slice(&verify.body).unwrap();
    assert_eq!(
        forwarded.op_hash, auth.op_hash,
        "signature forwarded verbatim"
    );
    assert_eq!(forwarded.sig, auth.sig);

    let capture = rec
        .iter()
        .find(|r| r.path == "/v1/holds/h1/capture")
        .expect("capture recorded");
    let cap_body: serde_json::Value = serde_json::from_slice(&capture.body).unwrap();
    assert_eq!(
        cap_body["amount_micros"], 11,
        "captured actual usage, not full price"
    );
    assert!(rec.iter().all(|r| !r.path.ends_with("/release")));

    // (7) REPLAY the same authorization → single-use nonce guard rejects → 402.
    let replay = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/echo")
                .header("aitp-authorization", header)
                .body(Body::from("hello world"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::PAYMENT_REQUIRED);
}
