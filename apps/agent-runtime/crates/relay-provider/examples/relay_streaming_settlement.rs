//! Offline example of the **primary Relay-billed STREAMING path** end to end.
//!
//! Run it with no network and no external keys:
//!
//! ```text
//! cd apps/agent-runtime && cargo run -p relay-provider --example relay_streaming_settlement
//! ```
//!
//! Unlike `agent-core`'s `minimal_agent` (which exercises the legacy fixed-price
//! direct `PaymentGateway` decorator), this example drives the real
//! `RelayModel::<PaidOpenAiCompletions>` streaming path where **Relay owns
//! pricing, reservation, usage settlement and evidence**. It reproduces, offline,
//! exactly the seam that `relay-provider`'s
//! `paid_stream_is_incremental_and_ends_after_durable_evidence` test covers:
//!
//!   1. The Rig OpenAI Chat Completions client streams a request through the
//!      AITP-aware HTTP backend.
//!   2. Relay answers HTTP `402` with a signed [`PaymentChallenge`]; the backend
//!      applies its policy, signs the identical request once, and retries.
//!   3. Relay streams SSE token deltas plus a trustworthy terminal `usage`.
//!   4. After the terminal usage, the backend looks up the durable Mint
//!      settlement receipt, cryptographically verifies the Mint attestation, and
//!      only then reports a clean terminal result.
//!   5. `drain_payment_evidence()` surfaces the verified Mint settlement.
//!
//! The Relay gateway and the Mint are an in-memory [`HttpTransport`] and an
//! ed25519 signing key — the same construction the crate's tests use. No stubs
//! of the model or the payment seam: the genuine `RelayModel` streaming code runs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use aitp_client::codec::{encode_challenge, HEADER_CHALLENGE, HEADER_REQUEST_ID};
use aitp_client::{
    AmountMicros, Ed25519Signer, HoldSettlementAttestation, HoldSettlementAttestationClaims,
    HttpRequest, HttpResponse, HttpTransport, PayError, PaymentChallenge, Receipt, Scheme,
    SettlementEvidenceVerifier, StreamingHttpResponse, HOLD_SETTLEMENT_ATTESTATION_FORMAT,
};
use async_trait::async_trait;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use bytes::Bytes;
use ed25519_dalek::{Signer as _, SigningKey};
use futures_util::{stream, StreamExt};
use relay_provider::{PaidOpenAiCompletions, RelayConfig, RelayModel};
use rig_core::completion::CompletionModel;
use rig_core::streaming::StreamedAssistantContent;
use sha2::{Digest, Sha256};

const REQUEST_ID: &str = "11111111-1111-4111-8111-111111111111";
/// Deterministic 32-byte seed the mock Mint signs settlement attestations with.
const MINT_SEED: [u8; 32] = [9; 32];
const MINT_KEY_ID: &str = "mint-devkey";

/// An in-memory Relay gateway + Mint. It issues one `402`, streams SSE tokens
/// with a terminal usage, and serves the durable Mint settlement receipt.
struct MockRelayAndMint {
    stream_calls: Mutex<usize>,
    lookup_calls: Mutex<usize>,
    challenge: PaymentChallenge,
    receipt: Receipt,
    evidence_available: AtomicBool,
}

impl MockRelayAndMint {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            stream_calls: Mutex::new(0),
            lookup_calls: Mutex::new(0),
            evidence_available: AtomicBool::new(true),
            challenge: PaymentChallenge {
                payee: "ait1qrelay".into(),
                amount_micros: AmountMicros(50_000),
                currency: "AIT".into(),
                resource:
                    "POST /openai/v1/chat/completions#openrouter/openai/gpt-4o@11111111-1111-4111-8111-111111111111"
                        .into(),
                nonce: "relay-stream-nonce".into(),
                expiry: i64::MAX,
                scheme: Scheme::Hold,
            },
            // The real settlement figure Relay commits to Mint after metering usage.
            receipt: Receipt {
                seq: 42,
                op: "hold.capture".into(),
                op_hash: "11".repeat(32),
                amount_micros: AmountMicros(750),
                balance_after_micros: AmountMicros(99_250),
                prev_hash: "22".repeat(32),
                hash: "33".repeat(32),
                sig: "44".repeat(64),
            },
        })
    }

    fn challenge_response(&self) -> Result<StreamingHttpResponse, PayError> {
        Ok(StreamingHttpResponse {
            status: 402,
            headers: vec![
                (
                    HEADER_CHALLENGE.to_ascii_lowercase(),
                    encode_challenge(&self.challenge)?,
                ),
                (HEADER_REQUEST_ID.to_ascii_lowercase(), REQUEST_ID.into()),
            ],
            body: Box::pin(stream::empty()),
        })
    }

    fn text_event(content: &str) -> String {
        format!(
            "data: {{\"id\":\"chatcmpl-stream\",\"model\":\"tensorzero::model_name::openai::gpt-4o\",\"choices\":[{{\"delta\":{{\"content\":\"{content}\",\"tool_calls\":[]}},\"finish_reason\":null}}],\"usage\":null}}\r\n\r\n"
        )
    }

    /// A realistic SSE body: token deltas, a terminal usage event, then `[DONE]`,
    /// deliberately split across chunk boundaries mid-event.
    fn success_chunks() -> Vec<Result<Bytes, PayError>> {
        let payload = format!(
            "{}{}{}{}",
            Self::text_event("Hello"),
            Self::text_event(", paid streaming!"),
            "data: {\"id\":\"chatcmpl-stream\",\"model\":\"tensorzero::model_name::openai::gpt-4o\",\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":50,\"total_tokens\":150}}\n\n",
            "data: [DONE]\n\n"
        )
        .into_bytes();
        let first = 73;
        let second = payload.len() - 47;
        vec![
            Ok(Bytes::copy_from_slice(&payload[..first])),
            Ok(Bytes::copy_from_slice(&payload[first..second])),
            Ok(Bytes::copy_from_slice(&payload[second..])),
        ]
    }
}

/// The mock Mint signs a hold-settlement attestation over the receipt, exactly
/// as the real Mint devserver would. `SettlementEvidenceVerifier` checks this.
fn signed_attestation(receipt: &Receipt) -> HoldSettlementAttestation {
    let claims = HoldSettlementAttestationClaims {
        version: 1,
        asset: "AIT".into(),
        unit: "micro_usd".into(),
        sequence: receipt.seq,
        operation: receipt.op.clone(),
        operation_hash: receipt.op_hash.clone(),
        amount_micros: receipt.amount_micros.get(),
        receipt_hash: receipt.hash.clone(),
        timestamp_ms: 1_700_000_000_000,
        reference_hash: "55".repeat(32),
        hold_commitment: "66".repeat(32),
        // Bind to the funding payer ("acct_payer"); the client rejects a proof
        // whose payer commitment does not match, exactly like the real Mint.
        payer_commitment: aitp_client::hold_party_commitment(
            "payer",
            "acct_payer",
            &receipt.op_hash,
        ),
        payee_commitment: "88".repeat(32),
        signer_key_id: MINT_KEY_ID.into(),
    };
    let payload = serde_json::to_vec(&claims).expect("claims serialize");
    let mut hasher = Sha256::new();
    hasher.update(b"mint-hold-settlement-attestation-v1\0");
    hasher.update(&payload);
    let digest: [u8; 32] = hasher.finalize().into();
    let signature = SigningKey::from_bytes(&MINT_SEED).sign(&digest);
    HoldSettlementAttestation {
        format: HOLD_SETTLEMENT_ATTESTATION_FORMAT.into(),
        payload_base64url: URL_SAFE_NO_PAD.encode(payload),
        digest_sha256: hex::encode(digest),
        signer_key_id: MINT_KEY_ID.into(),
        signature_hex: hex::encode(signature.to_bytes()),
    }
}

#[async_trait]
impl HttpTransport for MockRelayAndMint {
    /// Non-streaming path: the Mint settlement-evidence lookup after the stream.
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, PayError> {
        *self.lookup_calls.lock().unwrap() += 1;
        assert_eq!(
            request.path,
            format!("/v1/billing/settlements/{REQUEST_ID}"),
            "unexpected Mint lookup path"
        );
        if !self.evidence_available.load(Ordering::SeqCst) {
            return Ok(HttpResponse::new(404, Vec::new()));
        }
        let attestation = signed_attestation(&self.receipt);
        let body = serde_json::json!({
            "data": {
                "request_id": REQUEST_ID,
                "principal": "acct_api_owner",
                "receipt": {
                    "sequence": self.receipt.seq,
                    "operation": self.receipt.op,
                    "operation_hash": self.receipt.op_hash,
                    "amount_micros": self.receipt.amount_micros.get(),
                    "receipt_hash": self.receipt.hash,
                    "signature": self.receipt.sig
                },
                "attestation": attestation,
                "verified_at_ms": 1_699_999_999_999_i64,
                "settled_at_ms": 1_700_000_000_000_i64
            }
        });
        Ok(HttpResponse::new(
            200,
            serde_json::to_vec(&body).expect("settlement evidence serializes"),
        ))
    }

    /// Streaming path: first call gets a `402`, the signed retry gets tokens.
    async fn send_streaming(
        &self,
        _request: HttpRequest,
    ) -> Result<StreamingHttpResponse, PayError> {
        let call = {
            let mut n = self.stream_calls.lock().unwrap();
            *n += 1;
            *n
        };
        if call == 1 {
            return self.challenge_response();
        }
        Ok(StreamingHttpResponse {
            status: 200,
            headers: vec![
                ("content-type".into(), "text/event-stream".into()),
                (HEADER_REQUEST_ID.to_ascii_lowercase(), REQUEST_ID.into()),
            ],
            body: Box::pin(stream::iter(Self::success_chunks())),
        })
    }
}

fn mint_verifier() -> SettlementEvidenceVerifier {
    SettlementEvidenceVerifier::from_public_key_bytes(
        MINT_KEY_ID,
        SigningKey::from_bytes(&MINT_SEED).verifying_key().to_bytes(),
    )
    .expect("Mint verifier")
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let relay = MockRelayAndMint::new();

    // The real paid streaming Relay model, with the in-memory Relay+Mint injected
    // in place of a socket. Everything else matches a live deployment.
    let config = RelayConfig::model_name(
        "http://relay.local/openai/v1",
        "relay-api-key",
        "openai::gpt-4o",
    );
    let model: RelayModel<PaidOpenAiCompletions> = RelayModel::connect_paid_with_transport(
        &config,
        "acct_payer",                                            // AITP payer principal
        "ait1qrelay",                                            // expected payee
        AmountMicros(100_000),                                   // policy max reservation (µ$)
        Arc::new(Ed25519Signer::from_seed("payer-key", &[7; 32])), // payer's signing key
        mint_verifier(),                                         // trusts the mock Mint's key
        relay.clone(),
    )
    .expect("paid streaming Relay model builds");

    println!("=== Relay-billed streaming inference (offline) ===");
    println!("prompt> Say hello.\n");

    let request = model.completion_request("Say hello.").build();
    let mut response = model.stream(request).await.expect("stream opens");

    let mut assembled = String::new();
    let mut usage = None;
    print!("agent> ");
    use std::io::Write as _;
    let _ = std::io::stdout().flush();
    while let Some(item) = response.next().await {
        match item.expect("stream item") {
            StreamedAssistantContent::Text(chunk) => {
                print!("[{}]", chunk.text); // brackets make each streamed delta visible
                let _ = std::io::stdout().flush();
                assembled.push_str(&chunk.text);
            }
            StreamedAssistantContent::Final(final_response) => {
                usage = Some(final_response.usage);
            }
            _ => {}
        }
    }
    println!("\n\nassembled reply: {assembled:?}");

    let usage = usage.expect("terminal usage present");
    println!(
        "usage> prompt_tokens={} completion_tokens={} total_tokens={}",
        usage.prompt_tokens,
        usage.total_tokens - usage.prompt_tokens,
        usage.total_tokens,
    );

    // After the terminal usage the backend verified the Mint attestation. Surface
    // the durable settlement receipt (AITP-402 Mint settlement) it recovered.
    let evidence = model.drain_payment_evidence();
    println!("\n=== AITP-402 Mint settlement ===");
    assert_eq!(evidence.len(), 1, "exactly one verified settlement expected");
    let ev = &evidence[0];
    let receipt = ev.receipt.as_ref().expect("verified settlement receipt");
    println!("settlement> request_id={:?}", ev.request_id.as_deref());
    println!("settlement> payer={} owner_principal={:?}", ev.payer, ev.owner_principal.as_deref());
    println!(
        "settlement> receipt seq={} op={} amount_micros={} balance_after_micros={}",
        receipt.seq,
        receipt.op,
        receipt.amount_micros.get(),
        receipt.balance_after_micros.get(),
    );
    println!(
        "settlement> mint_attestation_present={} verified_at_ms={:?}",
        ev.attestation.is_some(),
        ev.verified_at_ms,
    );
    println!("settlement> receipt_hash={}", receipt.hash);

    // Invariants that must hold for money correctness.
    assert_eq!(ev.request_id.as_deref(), Some(REQUEST_ID));
    assert_eq!(ev.payer, "acct_payer");
    assert_eq!(receipt.amount_micros, AmountMicros(750));
    assert!(ev.attestation.is_some(), "Mint attestation must be verified");
    assert!(
        model.drain_payment_evidence().is_empty(),
        "evidence queue drains exactly once"
    );
    assert_eq!(*relay.stream_calls.lock().unwrap(), 2, "one 402 + one signed retry");
    assert_eq!(*relay.lookup_calls.lock().unwrap(), 1, "one Mint settlement lookup");

    println!("\nOK: streamed tokens delivered incrementally and the paid stream");
    println!("    ended only after a cryptographically verified Mint settlement.");
}
