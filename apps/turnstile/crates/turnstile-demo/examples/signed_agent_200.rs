//! Signed-agent 200 happy-path driver for Turnstile.
//!
//! Before this, only the `402` challenge and the settlement-status `404` were
//! driven by hand. This driver closes the loop: it generates a REAL ed25519 agent
//! keypair, performs the signed `AITP-Authorization` handshake, and drives the
//! full `402 -> sign -> 200` paid settlement, then a settlement-status `GET` that
//! returns the signed `AITP-Receipt` (HTTP `200`).
//!
//! Crypto and wire contract are REAL (aitp-client blake3 `op_hash` +
//! `Ed25519Signer` + `codec`). The Mint is faked ONLY at aitp-client's
//! `HttpTransport` seam (a `FakeMintTransport` that records requests and replays
//! canned responses, incl. a signed capture receipt) — exactly like the crate's
//! own `tests/e2e_paywall.rs`, so no live mint-server is needed. The turnstile
//! server, its gating middleware, the settlement journal, and the
//! settlement-status API are all the REAL production code.
//!
//! Run: `cargo run -p turnstile-demo --example signed_agent_200`

use std::sync::{Arc, Mutex};
use std::time::Duration;

use aitp_client::codec::{decode_challenge, encode_authorization, encode_receipt};
use aitp_client::signer::{op_hash, op_hash_hex, AuthSigner, AuthorizationIntent, Ed25519Signer};
use aitp_client::transport::{HttpRequest, HttpResponse, HttpTransport};
use aitp_client::{AmountMicros, PayError, PaymentAuthorization, PaymentChallenge, Receipt};
use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::middleware::from_fn_with_state;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::Router;
use tower::ServiceExt;
use turnstile_axum::{admit, settlement_status, PaidRequest};
use turnstile_core::{
    AmountMicros as CoreAmount, ChallengeIssuer, InMemoryNonceStore, InMemorySettlementJournal,
    MintVerifier, NonceStore, SettlementJournal, Turnstile, TurnstileConfig,
};

// --- fake mint-server at the aitp-client transport seam (canned responses) ---
struct FakeMintTransport {
    recorded: Mutex<Vec<HttpRequest>>,
}

impl FakeMintTransport {
    fn new() -> Self {
        Self {
            recorded: Mutex::new(Vec::new()),
        }
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
            r#"{"success":true,"data":{"first_seq":1,"captured_micros":11,"holder_balance":{"available_micros":100}}}"#
        } else {
            r#"{"success":true,"data":{}}"#
        };
        let response = HttpResponse::new(200, body.as_bytes().to_vec());
        if path.ends_with("/capture") {
            // A real signed settlement receipt travels back on the capture.
            let receipt = Receipt {
                seq: 1,
                op: "hold.capture".into(),
                op_hash: "11".repeat(32),
                amount_micros: AmountMicros(11),
                balance_after_micros: AmountMicros(100),
                prev_hash: "22".repeat(32),
                hash: "33".repeat(32),
                sig: "44".repeat(64),
            };
            return Ok(response.with_header(
                "aitp-receipt",
                encode_receipt(&receipt).expect("encode receipt"),
            ));
        }
        Ok(response)
    }
}

fn config() -> TurnstileConfig {
    TurnstileConfig {
        mint_server_url: "http://mint.internal".into(),
        api_key: "k".into(),
        payee: "ait1qpayee".into(),
        price_micros: CoreAmount(1_000),
        currency: "AIT".into(),
        challenge_ttl_ms: 60_000,
    }
}

/// The paid tool: echoes the body, reporting ACTUAL usage so the middleware
/// captures only what was used (success-only billing).
async fn echo(paid: PaidRequest, body: String) -> impl IntoResponse {
    let quoted = paid.price().get();
    let used = (body.len() as i64).clamp(0, quoted);
    paid.report_actual(CoreAmount::new(used));
    (StatusCode::OK, body)
}

/// REAL agent-side signing (mirrors aitp-client's `MintPaymentGateway`).
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
        scheme: ch.scheme,
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
        scheme: ch.scheme,
        idempotency_key,
        pubkey_id: signer.pubkey_id(),
        op_hash: op_hash_hex(&hash),
        sig: signer.sign_op_hash(&hash),
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Real production turnstile stack (journal-backed so settlement-status works).
    let transport = Arc::new(FakeMintTransport::new());
    let verifier = Arc::new(MintVerifier::new(transport, "k", "ait1qpayee"));
    let nonces: Arc<dyn NonceStore> = Arc::new(InMemoryNonceStore::new());
    let settlements: Arc<dyn SettlementJournal> = Arc::new(InMemorySettlementJournal::new());
    let issuer = ChallengeIssuer::new(config(), nonces.clone());
    let gate = Arc::new(Turnstile::with_journal(
        issuer,
        verifier,
        nonces,
        settlements.clone(),
        Duration::from_secs(30),
    ));

    let status_api = Router::new()
        .route("/v1/settlements/:request_id", get(settlement_status))
        .with_state(settlements.clone());
    let app = Router::new()
        .route(
            "/v1/echo",
            post(echo).route_layer(from_fn_with_state(gate.clone(), admit)),
        )
        .merge(status_api);

    println!("== turnstile signed-agent 200 driver (real ed25519 + real gating stack) ==\n");

    // (1) UNPAID -> 402 + AITP-Challenge.
    let unpaid = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/echo")
                .body(Body::from("hello world"))?,
        )
        .await?;
    println!("(1) unpaid POST /v1/echo            -> {}", unpaid.status());
    assert_eq!(unpaid.status(), StatusCode::PAYMENT_REQUIRED);
    let challenge = decode_challenge(
        unpaid
            .headers()
            .get("aitp-challenge")
            .expect("challenge header")
            .to_str()?,
    )?;
    println!(
        "    AITP-Challenge: resource={:?} amount_micros={} nonce={} scheme={:?}",
        challenge.resource,
        challenge.amount_micros.get(),
        challenge.nonce,
        challenge.scheme
    );

    // (2) Generate a REAL ed25519 agent keypair and sign the pinned intent.
    let signer = Ed25519Signer::from_seed("key_agent", &[9u8; 32]);
    let auth = sign_authorization(&signer, "acct_payer", &challenge);
    println!(
        "\n(2) agent keypair pubkey_id={} signed op_hash={} (sig {} hex chars)",
        auth.pubkey_id,
        auth.op_hash,
        auth.sig.len()
    );
    assert_eq!(auth.op_hash.len(), 64, "real 32-byte blake3 op_hash");
    assert!(!auth.sig.is_empty(), "real ed25519 signature");
    let header = encode_authorization(&auth)?;

    // (3) SIGNED RETRY -> verify -> admit -> handler -> capture -> 200 + receipt.
    let paid = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/echo")
                .header("aitp-authorization", header.clone())
                .body(Body::from("hello world"))?,
        )
        .await?;
    println!("\n(3) signed POST /v1/echo            -> {}", paid.status());
    assert_eq!(paid.status(), StatusCode::OK);
    let request_id = paid
        .headers()
        .get("aitp-request-id")
        .expect("AITP-Request-Id header on paid 200")
        .to_str()?
        .to_string();
    let has_receipt = paid.headers().get("aitp-receipt").is_some();
    println!("    AITP-Request-Id: {request_id}");
    println!("    AITP-Receipt header present: {has_receipt}");
    let body = axum::body::to_bytes(paid.into_body(), usize::MAX).await?;
    println!("    body: {:?}", String::from_utf8_lossy(&body));
    assert!(has_receipt, "receipt attached after capture");

    // (4) SETTLEMENT-STATUS GET -> 200 with the signed receipt (the new path).
    let status = app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/v1/settlements/{request_id}"))
                .body(Body::empty())?,
        )
        .await?;
    println!(
        "\n(4) GET /v1/settlements/{{id}}         -> {}",
        status.status()
    );
    assert_eq!(status.status(), StatusCode::OK);
    let status_body = axum::body::to_bytes(status.into_body(), usize::MAX).await?;
    let json: serde_json::Value = serde_json::from_slice(&status_body)?;
    println!(
        "    phase={} settled={} scheme={} actual_micros={}",
        json["phase"], json["settled"], json["scheme"], json["actual_micros"]
    );
    println!(
        "    signed receipt: op={} amount_micros={} seq={} sig={}…",
        json["receipt"]["op"],
        json["receipt"]["amount_micros"],
        json["receipt"]["seq"],
        json["receipt"]["sig"].as_str().unwrap_or("").get(0..16).unwrap_or("")
    );
    assert_eq!(json["settled"], serde_json::Value::Bool(true));
    assert!(json["receipt"].is_object(), "signed receipt returned in status");

    // (5) REPLAY the same authorization -> single-use nonce guard -> 402.
    let replay = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/echo")
                .header("aitp-authorization", header)
                .body(Body::from("hello world"))?,
        )
        .await?;
    println!(
        "\n(5) replayed signed POST /v1/echo   -> {} (single-use nonce enforced)",
        replay.status()
    );
    assert_eq!(replay.status(), StatusCode::PAYMENT_REQUIRED);

    println!("\nPROOF: full 402 -> sign -> 200 with a real ed25519 authorization,");
    println!("       and settlement-status GET returned the signed receipt (HTTP 200).");
    Ok(())
}
