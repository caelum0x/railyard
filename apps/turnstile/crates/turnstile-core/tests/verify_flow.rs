//! Faithful HTTP tests for [`MintVerifier`] driven over a fake that implements
//! aitp-client's real [`HttpTransport`] seam.
//!
//! The "fake mint-server" RECORDS every [`HttpRequest`] and REPLAYS canned
//! mint-server [`HttpResponse`]s keyed by the request path. Production swaps in
//! `ReqwestTransport` unchanged. These tests assert the EXACT wire the verifier
//! puts on the seam: path, method, `x-api-key`, and the JSON bodies for
//! `/v1/aitp/verify`, `/v1/addresses/:a`, `/v1/holds/:id/capture|release`, plus
//! the mint error-envelope → typed [`PayError`] mapping.

use std::sync::{Arc, Mutex};

use aitp_client::codec::encode_receipt;
use aitp_client::transport::{HttpRequest, HttpResponse, HttpTransport};
use aitp_client::{AmountMicros, HoldRef, PayError, PaymentAuthorization, Receipt, Scheme};
use async_trait::async_trait;
use turnstile_core::{MintVerifier, PaymentVerifier, TurnstileError};

// --- the injected fake mint-server -----------------------------------------

/// Records each outbound request and replays a canned response chosen by path.
struct FakeMintTransport {
    recorded: Mutex<Vec<HttpRequest>>,
    verify: HttpResponse,
    address: HttpResponse,
    capture: HttpResponse,
    release: HttpResponse,
}

impl FakeMintTransport {
    /// A transport whose every route returns a success envelope.
    fn happy() -> Self {
        Self {
            recorded: Mutex::new(Vec::new()),
            verify: json_ok(r#"{"success":true,"data":{"scheme":"hold","hold_id":"h1"}}"#),
            address: json_ok(r#"{"success":true,"data":{"account_id":"acct_payee"}}"#),
            capture: json_ok(
                r#"{"success":true,"data":{"first_seq":7,"captured_micros":3900,"holder_balance":{"available_micros":96100}}}"#,
            )
            .with_header("aitp-receipt", encoded_capture_receipt()),
            release: json_ok(r#"{"success":true,"data":{}}"#),
        }
    }

    /// Override the `/v1/aitp/verify` response (e.g. an error envelope).
    fn with_verify(mut self, resp: HttpResponse) -> Self {
        self.verify = resp;
        self
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
        let resp = if path == "/v1/aitp/verify" {
            self.verify.clone()
        } else if path.starts_with("/v1/addresses/") {
            self.address.clone()
        } else if path.ends_with("/capture") {
            self.capture.clone()
        } else if path.ends_with("/release") {
            self.release.clone()
        } else {
            json_ok(r#"{"success":false,"error":{"code":"not_found"}}"#)
        };
        Ok(resp)
    }
}

fn json_ok(body: &str) -> HttpResponse {
    HttpResponse::new(200, body.as_bytes().to_vec())
}

fn json_err(status: u16, body: &str) -> HttpResponse {
    HttpResponse::new(status, body.as_bytes().to_vec())
}

fn encoded_capture_receipt() -> String {
    encode_receipt(&Receipt {
        seq: 7,
        op: "hold.capture".to_string(),
        op_hash: "11".repeat(32),
        amount_micros: AmountMicros(3900),
        balance_after_micros: AmountMicros(96_100),
        prev_hash: "22".repeat(32),
        hash: "33".repeat(32),
        sig: "44".repeat(64),
    })
    .unwrap()
}

fn sample_auth() -> PaymentAuthorization {
    PaymentAuthorization {
        payer: "acct_payer".into(),
        payee: "ait1qpayee".into(),
        amount_micros: AmountMicros(4200),
        nonce: "9f2c".into(),
        resource: "POST /v1/echo".into(),
        expiry: 9_999_999_999_999,
        scheme: aitp_client::Scheme::Hold,
        idempotency_key: "aitp:9f2c".into(),
        pubkey_id: "key_1".into(),
        op_hash: "deadbeef".into(),
        sig: "sig".into(),
    }
}

// --- tests ------------------------------------------------------------------

#[tokio::test]
async fn verify_forwards_flat_authorization_with_api_key() {
    let fake = Arc::new(FakeMintTransport::happy());
    let verifier = MintVerifier::new(fake.clone(), "secret-key", "ait1qpayee");

    let auth = sample_auth();
    let verified = verifier.verify(&auth).await.unwrap();

    assert_eq!(verified.scheme, Scheme::Hold);
    assert_eq!(verified.hold, Some(HoldRef("h1".into())));

    let rec = fake.recorded();
    assert_eq!(rec.len(), 1, "verify makes exactly one mint call");
    assert_eq!(rec[0].method, "POST");
    assert_eq!(rec[0].path, "/v1/aitp/verify");
    assert!(
        rec[0]
            .headers
            .iter()
            .any(|(k, v)| k == "x-api-key" && v == "secret-key"),
        "x-api-key must be attached"
    );
    // Body is the flat PaymentAuthorization JSON, values preserved so the mint
    // can recompute op_hash and ed25519-verify.
    assert_eq!(rec[0].body, serde_json::to_vec(&auth).unwrap());
}

#[tokio::test]
async fn verify_maps_signature_invalid_and_makes_no_capture() {
    let fake = Arc::new(FakeMintTransport::happy().with_verify(json_err(
        400,
        r#"{"success":false,"error":{"code":"signature_invalid","message":"bad sig"}}"#,
    )));
    let verifier = MintVerifier::new(fake.clone(), "k", "ait1qpayee");

    let err = verifier.verify(&sample_auth()).await.unwrap_err();
    assert!(matches!(
        err,
        TurnstileError::Pay(PayError::SignatureInvalid)
    ));
    assert!(
        fake.recorded()
            .iter()
            .all(|r| !r.path.ends_with("/capture")),
        "a rejected verify must never capture"
    );
}

#[tokio::test]
async fn verify_maps_insufficient_funds() {
    let fake = Arc::new(FakeMintTransport::happy().with_verify(json_err(
        402,
        r#"{"success":false,"error":{"code":"insufficient_funds"}}"#,
    )));
    let verifier = MintVerifier::new(fake.clone(), "k", "ait1qpayee");

    let err = verifier.verify(&sample_auth()).await.unwrap_err();
    assert!(matches!(
        err,
        TurnstileError::Pay(PayError::InsufficientFunds)
    ));
}

#[tokio::test]
async fn capture_resolves_payee_then_captures_actual() {
    let fake = Arc::new(FakeMintTransport::happy());
    let verifier = MintVerifier::new(fake.clone(), "k", "ait1qpayee");

    let receipt = verifier
        .capture(&HoldRef("h1".into()), AmountMicros(3900))
        .await
        .unwrap();
    assert_eq!(receipt.amount_micros, AmountMicros(3900));
    assert_eq!(receipt.balance_after_micros, AmountMicros(96_100));
    assert_eq!(receipt.op_hash, "11".repeat(32));
    assert_eq!(receipt.hash, "33".repeat(32));
    assert_eq!(receipt.sig, "44".repeat(64));

    let rec = fake.recorded();
    // Address resolution first...
    assert_eq!(rec[0].method, "GET");
    assert_eq!(rec[0].path, "/v1/addresses/ait1qpayee");
    // ...then the capture to the resolved account.
    let cap = rec
        .iter()
        .find(|r| r.path == "/v1/holds/h1/capture")
        .expect("capture call recorded");
    assert_eq!(cap.method, "POST");
    let body: serde_json::Value = serde_json::from_slice(&cap.body).unwrap();
    assert_eq!(body["amount_micros"], 3900);
    assert_eq!(body["to"], "acct_payee");
    assert_eq!(body["idem_key"], "aitp:h1:capture");
}

#[tokio::test]
async fn release_frees_hold_without_capture() {
    let fake = Arc::new(FakeMintTransport::happy());
    let verifier = MintVerifier::new(fake.clone(), "k", "ait1qpayee");

    verifier.release(&HoldRef("h1".into())).await.unwrap();

    let rec = fake.recorded();
    let rel = rec
        .iter()
        .find(|r| r.path == "/v1/holds/h1/release")
        .expect("release call recorded");
    assert_eq!(rel.method, "POST");
    let body: serde_json::Value = serde_json::from_slice(&rel.body).unwrap();
    assert_eq!(body["idem_key"], "aitp:h1:release");
    assert!(
        rec.iter().all(|r| !r.path.ends_with("/capture")),
        "release must never capture"
    );
}
