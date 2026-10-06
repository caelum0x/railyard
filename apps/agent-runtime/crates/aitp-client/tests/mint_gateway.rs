//! Integration tests for the REAL `MintPaymentGateway` against a FAKE HTTP
//! transport (no live server, no network). The fake replays canned mint-server
//! responses in order and records every request so each test asserts the EXACT
//! path, headers, and JSON body the gateway sent.
//!
//! Coverage: authorize verifies + holds; success captures; failure releases;
//! typed error propagation. The signing path is real (ed25519 + blake3), so the
//! recorded `op_hash`/`sig` are genuine, not stubbed.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use aitp_client::codec::encode_receipt;
use aitp_client::{
    AmountMicros, AuthSigner, Ed25519Signer, HttpRequest, HttpResponse, HttpTransport,
    MintPaymentGateway, PayError, PaymentChallenge, PaymentGateway, Scheme,
};
use async_trait::async_trait;

const API_KEY: &str = "test-api-key";
const PAYER: &str = "acct_payer_01";
const PAYEE: &str = "ait1qprovider";
const NONCE: &str = "9f2c";

/// A fake [`HttpTransport`]: pops queued responses in FIFO order and records
/// every request for assertion.
struct FakeTransport {
    responses: Mutex<VecDeque<HttpResponse>>,
    requests: Mutex<Vec<HttpRequest>>,
}

impl FakeTransport {
    fn new(responses: Vec<HttpResponse>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into()),
            requests: Mutex::new(Vec::new()),
        })
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl HttpTransport for FakeTransport {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, PayError> {
        self.requests.lock().unwrap().push(req.clone());
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| PayError::Gateway(format!("fake: no canned response for {}", req.path)))
    }
}

fn signer() -> Arc<dyn AuthSigner> {
    Arc::new(Ed25519Signer::from_seed("key_payer_1", &[7u8; 32]))
}

fn hold_challenge() -> PaymentChallenge {
    PaymentChallenge {
        payee: PAYEE.into(),
        amount_micros: AmountMicros(4200),
        currency: "AIT".into(),
        resource: "POST /v1/chat/completions#llama-3.1-70b".into(),
        nonce: NONCE.into(),
        expiry: 1_723_958_400_000,
        scheme: Scheme::Hold,
    }
}

fn verify_hold_ok() -> HttpResponse {
    HttpResponse::new(
        200,
        br#"{"success":true,"data":{"scheme":"hold","hold_id":"hold_01H","receipt":{"hold_id":"hold_01H","first_seq":11,"status":"open"}}}"#.to_vec(),
    )
    .with_header("aitp-version", "0.1")
}

fn address_ok() -> HttpResponse {
    HttpResponse::new(
        200,
        br#"{"success":true,"data":{"account_id":"acct_provider_9"}}"#.to_vec(),
    )
}

fn capture_ok() -> HttpResponse {
    let receipt = aitp_client::Receipt {
        seq: 42,
        op: "hold.capture".to_string(),
        op_hash: "11".repeat(32),
        amount_micros: AmountMicros(3900),
        balance_after_micros: AmountMicros(1300),
        prev_hash: "22".repeat(32),
        hash: "33".repeat(32),
        sig: "44".repeat(64),
    };
    HttpResponse::new(
        200,
        br#"{"success":true,"data":{"hold_id":"hold_01H","first_seq":42,"holder_balance":{"account_id":"acct_payer_01","available_micros":1300,"held_micros":0,"updated_seq":42},"payee_balance":null,"status":"captured","captured_micros":3900,"idempotent_replay":false}}"#.to_vec(),
    )
    .with_header("aitp-receipt", encode_receipt(&receipt).unwrap())
}

fn release_ok() -> HttpResponse {
    HttpResponse::new(
        200,
        br#"{"success":true,"data":{"hold_id":"hold_01H","first_seq":43,"holder_balance":{"account_id":"acct_payer_01","available_micros":4200,"held_micros":0,"updated_seq":43},"payee_balance":null,"status":"released","captured_micros":0,"idempotent_replay":false}}"#.to_vec(),
    )
}

fn body_json(req: &HttpRequest) -> serde_json::Value {
    serde_json::from_slice(&req.body).expect("request body is JSON")
}

fn header<'a>(req: &'a HttpRequest, name: &str) -> Option<&'a str> {
    req.headers
        .iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.as_str())
}

#[tokio::test]
async fn authorize_posts_signed_verify_and_returns_a_hold() {
    let transport = FakeTransport::new(vec![verify_hold_ok()]);
    let gw = MintPaymentGateway::new(PAYER, API_KEY, transport.clone(), signer());

    let authorized = gw.authorize(&hold_challenge()).await.unwrap();

    // A hold challenge yields a HoldRef from the response body's data.hold_id.
    assert_eq!(authorized.hold.as_ref().unwrap().0, "hold_01H");

    // Exactly one request: POST /v1/aitp/verify with the API key.
    let reqs = transport.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].method, "POST");
    assert_eq!(reqs[0].path, "/v1/aitp/verify");
    assert_eq!(header(&reqs[0], "x-api-key"), Some(API_KEY));

    // The body is the flat authorization the gateway signed.
    let body = body_json(&reqs[0]);
    assert_eq!(body["payer"], PAYER);
    assert_eq!(body["payee"], PAYEE);
    assert_eq!(body["amount_micros"], 4200);
    assert_eq!(body["nonce"], NONCE);
    assert_eq!(body["resource"], "POST /v1/chat/completions#llama-3.1-70b");
    assert_eq!(body["idempotency_key"], format!("aitp:{NONCE}"));
    assert_eq!(body["pubkey_id"], "key_payer_1");

    // The op_hash is a real 64-char blake3 hex and the sig is non-empty — the
    // returned authorization carries the same values.
    let op_hash = body["op_hash"].as_str().unwrap();
    assert_eq!(op_hash.len(), 64);
    assert!(op_hash.chars().all(|c| c.is_ascii_hexdigit()));
    assert!(!body["sig"].as_str().unwrap().is_empty());
    assert_eq!(authorized.authorization.op_hash, op_hash);
}

#[tokio::test]
async fn success_path_captures_actual_to_the_resolved_payee() {
    // authorize → capture: verify, then address lookup, then hold capture.
    let transport = FakeTransport::new(vec![verify_hold_ok(), address_ok(), capture_ok()]);
    let gw = MintPaymentGateway::new(PAYER, API_KEY, transport.clone(), signer());

    let authorized = gw.authorize(&hold_challenge()).await.unwrap();
    let hold = authorized.hold.unwrap();

    let receipt = gw.capture(&hold, AmountMicros(3900)).await.unwrap();

    // Receipt is the signed evidence returned by Mint, not reconstructed from
    // the mutable balance view.
    assert_eq!(receipt.seq, 42);
    assert_eq!(receipt.op, "hold.capture");
    assert_eq!(receipt.amount_micros, AmountMicros(3900));
    assert_eq!(receipt.balance_after_micros, AmountMicros(1300));
    assert_eq!(receipt.op_hash, "11".repeat(32));
    assert_eq!(receipt.hash, "33".repeat(32));
    assert_eq!(receipt.sig, "44".repeat(64));

    let reqs = transport.requests();
    assert_eq!(reqs.len(), 3);

    // (2) address lookup resolves the ait1 payee into an AccountId.
    assert_eq!(reqs[1].method, "GET");
    assert_eq!(reqs[1].path, format!("/v1/addresses/{PAYEE}"));

    // (3) capture posts to the hold path with the resolved `to` and actual amt.
    assert_eq!(reqs[2].method, "POST");
    assert_eq!(reqs[2].path, "/v1/holds/hold_01H/capture");
    assert_eq!(header(&reqs[2], "x-api-key"), Some(API_KEY));
    let body = body_json(&reqs[2]);
    assert_eq!(body["amount_micros"], 3900);
    assert_eq!(body["to"], "acct_provider_9");
    assert_eq!(body["idem_key"], format!("aitp:{NONCE}:capture"));
}

#[tokio::test]
async fn failure_path_releases_the_hold_without_settling() {
    let transport = FakeTransport::new(vec![verify_hold_ok(), release_ok()]);
    let gw = MintPaymentGateway::new(PAYER, API_KEY, transport.clone(), signer());

    let authorized = gw.authorize(&hold_challenge()).await.unwrap();
    let hold = authorized.hold.unwrap();

    gw.release(&hold).await.unwrap();

    let reqs = transport.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[1].method, "POST");
    assert_eq!(reqs[1].path, "/v1/holds/hold_01H/release");
    assert_eq!(header(&reqs[1], "x-api-key"), Some(API_KEY));
    let body = body_json(&reqs[1]);
    assert_eq!(body["idem_key"], format!("aitp:{NONCE}:release"));
}

#[tokio::test]
async fn authorize_maps_a_known_error_code_to_a_typed_pay_error() {
    let transport = FakeTransport::new(vec![HttpResponse::new(
        402,
        br#"{"success":false,"error":{"code":"insufficient_funds","message":"balance too low"}}"#
            .to_vec(),
    )]);
    let gw = MintPaymentGateway::new(PAYER, API_KEY, transport.clone(), signer());

    let err = gw.authorize(&hold_challenge()).await.unwrap_err();
    assert!(matches!(err, PayError::InsufficientFunds), "got {err:?}");
}

#[tokio::test]
async fn authorize_maps_signature_invalid_from_the_verifier() {
    let transport = FakeTransport::new(vec![HttpResponse::new(
        422,
        br#"{"success":false,"error":{"code":"signature_invalid","message":"bad sig"}}"#.to_vec(),
    )]);
    let gw = MintPaymentGateway::new(PAYER, API_KEY, transport.clone(), signer());

    let err = gw.authorize(&hold_challenge()).await.unwrap_err();
    assert!(matches!(err, PayError::SignatureInvalid), "got {err:?}");
}

#[tokio::test]
async fn unknown_error_code_falls_back_to_gateway() {
    let transport = FakeTransport::new(vec![HttpResponse::new(
        400,
        br#"{"success":false,"error":{"code":"teapot","message":"nope"}}"#.to_vec(),
    )]);
    let gw = MintPaymentGateway::new(PAYER, API_KEY, transport.clone(), signer());

    let err = gw.authorize(&hold_challenge()).await.unwrap_err();
    assert!(matches!(err, PayError::Gateway(_)), "got {err:?}");
}

#[tokio::test]
async fn transfer_scheme_settles_immediately_with_no_hold() {
    let transport = FakeTransport::new(vec![HttpResponse::new(
        200,
        br#"{"success":true,"data":{"scheme":"transfer","receipt":{"first_seq":5}}}"#.to_vec(),
    )]);
    let gw = MintPaymentGateway::new(PAYER, API_KEY, transport.clone(), signer());

    let mut challenge = hold_challenge();
    challenge.scheme = Scheme::Transfer;

    let authorized = gw.authorize(&challenge).await.unwrap();
    assert!(authorized.hold.is_none());
}

/// A hold's capture context survives a restart: exported from the gateway
/// that authorized it, restored into a fresh one, which then captures with the
/// same payee and the same deterministic idempotency key.
#[tokio::test]
async fn a_restored_hold_context_captures_like_the_original() {
    let first = FakeTransport::new(vec![verify_hold_ok()]);
    let gw = MintPaymentGateway::new(PAYER, API_KEY, first.clone(), signer());
    let auth = gw.authorize(&hold_challenge()).await.unwrap();
    let hold = auth.hold.unwrap();
    let context = gw.hold_context(&hold).expect("a hold has a context");
    assert_eq!(context["payee"], PAYEE);

    // "Restart": a new gateway knows nothing about the hold until restored.
    let second = FakeTransport::new(vec![
        HttpResponse::new(200, br#"{"success":true,"data":{"account_id":"acct_provider_01"}}"#.to_vec()),
        HttpResponse::new(200, br#"{"success":true,"data":{}}"#.to_vec()),
    ]);
    let restarted = MintPaymentGateway::new(PAYER, API_KEY, second.clone(), signer());
    assert!(restarted.capture(&hold, AmountMicros(4200)).await.is_err(), "no context, no capture");
    restarted.restore_hold_context(&hold, &context).unwrap();
    let _ = restarted.capture(&hold, AmountMicros(4200)).await;
    let reqs = second.requests();
    let capture = reqs.iter().find(|r| r.path.ends_with("/capture")).expect("capture sent");
    let body: serde_json::Value = serde_json::from_slice(&capture.body).unwrap();
    assert_eq!(body["to"], "acct_provider_01");
    assert!(body["idem_key"].as_str().unwrap().ends_with(":capture"));
}
