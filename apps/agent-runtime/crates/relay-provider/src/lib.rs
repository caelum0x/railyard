//! `relay-provider`: a rig [`rig_core::completion::CompletionModel`] backed by
//! Relay's OpenAI-compatible (TensorZero) gateway.
//!
//! The Rig reuse lives here: [`RelayModel`]
//! wraps Rig's open-source OpenAI Chat Completions client pointed at Relay. Tests wrap a
//! `MockCompletionModel` instead so nothing touches the network.

pub mod config;
pub mod error;
pub mod model;
pub mod paid_transport;

pub use config::{RelayConfig, FUNCTION_NAME_PREFIX, MODEL_NAME_PREFIX, OPENAI_COMPAT_SUFFIX};
pub use error::RelayError;
pub use model::{OpenAiCompletions, PaidOpenAiCompletions, RelayModel};
pub use paid_transport::AitpRigHttpClient;

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;
    use aitp_client::codec::{
        decode_authorization, encode_challenge, HEADER_AUTHORIZATION, HEADER_CHALLENGE,
        HEADER_REQUEST_ID, HEADER_VERSION,
    };
    use aitp_client::{
        AmountMicros, Ed25519Signer, HoldSettlementAttestation, HoldSettlementAttestationClaims,
        HttpRequest, HttpResponse, HttpTransport, PayError, PaymentChallenge, Receipt, Scheme,
        SettlementEvidenceVerifier, StreamingHttpResponse,
    };
    use async_trait::async_trait;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use bytes::Bytes;
    use ed25519_dalek::{Signer as _, SigningKey};
    use futures_util::{stream, StreamExt};
    use rig_core::completion::{AssistantContent, CompletionModel};
    use rig_core::http_client::{HttpClientExt, MultipartForm, Request};
    use rig_core::streaming::StreamedAssistantContent;
    use rig_core::test_utils::MockCompletionModel;
    use sha2::{Digest, Sha256};

    #[derive(Clone, Copy)]
    enum PaidFlow {
        ChallengeThenOk,
        Funded,
        ChallengeTwice,
    }

    struct RecordingRelay {
        flow: PaidFlow,
        seen: Mutex<Vec<HttpRequest>>,
        challenge: PaymentChallenge,
    }

    #[derive(Clone, Copy)]
    enum StreamingFlow {
        Success,
        MissingUsage,
        MissingEvidence,
        TamperedEvidence,
        ApplicationError,
        BodyError,
        ChallengeTwice,
    }

    struct StreamingRelay {
        flow: StreamingFlow,
        stream_seen: Mutex<Vec<HttpRequest>>,
        lookup_seen: Mutex<Vec<HttpRequest>>,
        challenge: PaymentChallenge,
        receipt: Receipt,
        evidence_available: AtomicBool,
    }

    impl StreamingRelay {
        fn new(flow: StreamingFlow) -> Arc<Self> {
            Arc::new(Self {
                flow,
                stream_seen: Mutex::new(Vec::new()),
                lookup_seen: Mutex::new(Vec::new()),
                evidence_available: AtomicBool::new(!matches!(
                    flow,
                    StreamingFlow::MissingEvidence
                )),
                challenge: PaymentChallenge {
                    payee: "ait1qrelay".into(),
                    amount_micros: AmountMicros(50_000),
                    currency: "AIT".into(),
                    resource: "POST /openai/v1/chat/completions#openrouter/openai/gpt-4o@11111111-1111-4111-8111-111111111111".into(),
                    nonce: "relay-stream-nonce".into(),
                    expiry: i64::MAX,
                    scheme: Scheme::Hold,
                },
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
                    (
                        HEADER_REQUEST_ID.to_ascii_lowercase(),
                        "11111111-1111-4111-8111-111111111111".into(),
                    ),
                ],
                body: Box::pin(stream::empty()),
            })
        }

        fn text_event(content: &str) -> String {
            format!(
                "data: {{\"id\":\"chatcmpl-stream\",\"model\":\"tensorzero::model_name::openai::gpt-4o\",\"choices\":[{{\"delta\":{{\"content\":\"{content}\",\"tool_calls\":[]}},\"finish_reason\":null}}],\"usage\":null}}\r\n\r\n"
            )
        }

        fn successful_chunks(&self) -> Vec<Result<Bytes, PayError>> {
            let payload = format!(
                "{}{}{}{}",
                Self::text_event("hel"),
                Self::text_event("lo"),
                "data: {\"id\":\"chatcmpl-stream\",\"model\":\"tensorzero::model_name::openai::gpt-4o\",\"choices\":[],\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":50,\"total_tokens\":150}}\n\n",
                "data: [DONE]\n\n"
            )
            .into_bytes();
            // Deliberately split in the middle of SSE fields/events. The
            // adapter and Rig parser must preserve incremental wire chunks
            // while validating complete events across arbitrary boundaries.
            let first = 73;
            let second = payload.len() - 47;
            vec![
                Ok(Bytes::copy_from_slice(&payload[..first])),
                Ok(Bytes::copy_from_slice(&payload[first..second])),
                Ok(Bytes::copy_from_slice(&payload[second..])),
            ]
        }
    }

    fn mint_verifier() -> SettlementEvidenceVerifier {
        SettlementEvidenceVerifier::from_public_key_bytes(
            "mint-test-key",
            SigningKey::from_bytes(&[9; 32]).verifying_key().to_bytes(),
        )
        .expect("test Mint verifier")
    }

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
            // Bind the attestation to the funding payer ("acct_payer"); the
            // client rejects proofs whose payer commitment does not match.
            payer_commitment: aitp_client::hold_party_commitment(
                "payer",
                "acct_payer",
                &receipt.op_hash,
            ),
            payee_commitment: "88".repeat(32),
            signer_key_id: "mint-test-key".into(),
        };
        let payload = serde_json::to_vec(&claims).expect("claims serialize");
        let mut hasher = Sha256::new();
        hasher.update(b"mint-hold-settlement-attestation-v1\0");
        hasher.update(&payload);
        let digest: [u8; 32] = hasher.finalize().into();
        let signature = SigningKey::from_bytes(&[9; 32]).sign(&digest);
        HoldSettlementAttestation {
            format: aitp_client::HOLD_SETTLEMENT_ATTESTATION_FORMAT.into(),
            payload_base64url: URL_SAFE_NO_PAD.encode(payload),
            digest_sha256: hex::encode(digest),
            signer_key_id: "mint-test-key".into(),
            signature_hex: hex::encode(signature.to_bytes()),
        }
    }

    #[async_trait]
    impl HttpTransport for StreamingRelay {
        async fn send(&self, request: HttpRequest) -> Result<HttpResponse, PayError> {
            self.lookup_seen
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(request.clone());
            assert_eq!(
                request.path,
                "/v1/billing/settlements/11111111-1111-4111-8111-111111111111"
            );
            if !self.evidence_available.load(Ordering::SeqCst) {
                return Ok(HttpResponse::new(404, Vec::new()));
            }
            let attestation = signed_attestation(&self.receipt);
            let mut projected_receipt = self.receipt.clone();
            if matches!(self.flow, StreamingFlow::TamperedEvidence) {
                projected_receipt.amount_micros = AmountMicros(751);
            }
            Ok(HttpResponse::new(
                200,
                serde_json::to_vec(&serde_json::json!({
                    "data": {
                        "request_id": "11111111-1111-4111-8111-111111111111",
                        "principal": "acct_api_owner",
                        "receipt": {
                            "sequence": projected_receipt.seq,
                            "operation": projected_receipt.op,
                            "operation_hash": projected_receipt.op_hash,
                            "amount_micros": projected_receipt.amount_micros.get(),
                            "receipt_hash": projected_receipt.hash,
                            "signature": projected_receipt.sig
                        },
                        "attestation": attestation,
                        "verified_at_ms": 1_699_999_999_999_i64,
                        "settled_at_ms": 1_700_000_000_000_i64
                    }
                }))
                .expect("settlement evidence serializes"),
            ))
        }

        async fn send_streaming(
            &self,
            request: HttpRequest,
        ) -> Result<StreamingHttpResponse, PayError> {
            let mut seen = self
                .stream_seen
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            seen.push(request);
            let call = seen.len();
            drop(seen);
            if call == 1 || matches!(self.flow, StreamingFlow::ChallengeTwice) {
                return self.challenge_response();
            }

            let chunks = match self.flow {
                StreamingFlow::Success
                | StreamingFlow::MissingEvidence
                | StreamingFlow::TamperedEvidence => self.successful_chunks(),
                StreamingFlow::MissingUsage => vec![
                    Ok(Bytes::from(Self::text_event("hel"))),
                    Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                ],
                StreamingFlow::ApplicationError => vec![
                    Ok(Bytes::from(Self::text_event("hel"))),
                    Ok(Bytes::from_static(
                        b"data: {\"error\":{\"message\":\"provider failed\"}}\n\n",
                    )),
                    Ok(Bytes::from_static(b"data: [DONE]\n\n")),
                ],
                StreamingFlow::BodyError => vec![
                    Ok(Bytes::from(Self::text_event("hel"))),
                    Err(PayError::Gateway(
                        "connection reset after partial body".into(),
                    )),
                ],
                StreamingFlow::ChallengeTwice => unreachable!(),
            };
            Ok(StreamingHttpResponse {
                status: 200,
                headers: vec![
                    ("content-type".into(), "text/event-stream".into()),
                    (
                        HEADER_REQUEST_ID.to_ascii_lowercase(),
                        "11111111-1111-4111-8111-111111111111".into(),
                    ),
                ],
                body: Box::pin(stream::iter(chunks)),
            })
        }
    }

    impl RecordingRelay {
        fn response_body() -> Vec<u8> {
            serde_json::to_vec(&serde_json::json!({
                "id": "chatcmpl-paid-1",
                "object": "chat.completion",
                "created": 1_700_000_000,
                "model": "tensorzero::model_name::openai::gpt-4o",
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": "paid relay works"},
                    "finish_reason": "stop"
                }],
                "usage": {
                    "prompt_tokens": 100,
                    "completion_tokens": 50,
                    "total_tokens": 150
                }
            }))
            .unwrap()
        }
    }

    #[async_trait]
    impl HttpTransport for RecordingRelay {
        async fn send(&self, request: HttpRequest) -> Result<HttpResponse, PayError> {
            let mut seen = self.seen.lock().unwrap_or_else(|error| error.into_inner());
            seen.push(request);
            let call = seen.len();
            match (self.flow, call) {
                (PaidFlow::Funded, 1) | (PaidFlow::ChallengeThenOk, 2) => {
                    Ok(HttpResponse::new(200, Self::response_body())
                        .with_header(HEADER_REQUEST_ID, "11111111-1111-4111-8111-111111111111"))
                }
                (PaidFlow::ChallengeThenOk, 1)
                | (PaidFlow::ChallengeTwice, 1)
                | (PaidFlow::ChallengeTwice, 2) => Ok(HttpResponse::new(402, Vec::new())
                    .with_header(HEADER_CHALLENGE, encode_challenge(&self.challenge)?)
                    .with_header(HEADER_REQUEST_ID, "11111111-1111-4111-8111-111111111111")),
                _ => panic!("unexpected Relay transport call {call}"),
            }
        }
    }

    fn paid_config() -> RelayConfig {
        RelayConfig::model_name(
            "http://relay.test/openai/v1",
            "relay-api-key",
            "openai::gpt-4o",
        )
    }

    fn paid_transport(flow: PaidFlow) -> Arc<RecordingRelay> {
        Arc::new(RecordingRelay {
            flow,
            seen: Mutex::new(Vec::new()),
            challenge: PaymentChallenge {
                payee: "ait1qrelay".into(),
                amount_micros: AmountMicros(50_000),
                currency: "AIT".into(),
                resource: "POST /openai/v1/chat/completions#openrouter/openai/gpt-4o@11111111-1111-4111-8111-111111111111".into(),
                nonce: "relay-paid-nonce".into(),
                expiry: i64::MAX,
                scheme: Scheme::Hold,
            },
        })
    }

    fn paid_model(transport: Arc<RecordingRelay>) -> RelayModel<PaidOpenAiCompletions> {
        RelayModel::connect_paid_with_transport(
            &paid_config(),
            "acct_payer",
            "ait1qrelay",
            AmountMicros(100_000),
            Arc::new(Ed25519Signer::from_seed("payer-key", &[7; 32])),
            mint_verifier(),
            transport,
        )
        .expect("paid Relay model builds")
    }

    fn paid_streaming_model(transport: Arc<StreamingRelay>) -> RelayModel<PaidOpenAiCompletions> {
        RelayModel::connect_paid_with_transport(
            &paid_config(),
            "acct_payer",
            "ait1qrelay",
            AmountMicros(100_000),
            Arc::new(Ed25519Signer::from_seed("payer-key", &[7; 32])),
            mint_verifier(),
            transport,
        )
        .expect("paid streaming Relay model builds")
    }

    #[test]
    fn model_name_is_relay_prefixed() {
        let cfg = RelayConfig::model_name("http://localhost/openai/v1", "k", "openai::gpt-4o-mini");
        assert_eq!(cfg.model, "tensorzero::model_name::openai::gpt-4o-mini");
    }

    #[test]
    fn connect_builds_a_live_model() {
        // No network: constructing the client does not call Relay.
        let cfg = RelayConfig::model_name("http://localhost:3000/openai/v1", "test-key", "dummy");
        let model = RelayModel::connect(&cfg).expect("client builds");
        let _ = model.inner();
    }

    #[tokio::test]
    async fn wraps_a_mock_for_offline_completion() {
        let mock = MockCompletionModel::text("hello from relay");
        let model = RelayModel::from_inner(mock);
        // The wrapper is a CompletionModel: build a request and complete it,
        // entirely offline.
        let request = model.completion_request("hi").build();
        let response = model.completion(request).await.expect("mock replies");
        let text = response.choice.iter().find_map(|c| match c {
            AssistantContent::Text(t) => Some(t.text.clone()),
            _ => None,
        });
        assert_eq!(text.as_deref(), Some("hello from relay"));
    }

    #[tokio::test]
    async fn high_level_model_signs_one_402_and_preserves_the_rig_request() {
        let transport = paid_transport(PaidFlow::ChallengeThenOk);
        let model = paid_model(transport.clone());
        let request = model.completion_request("hello").build();
        let response = model.completion(request).await.expect("paid completion");
        assert_eq!(response.usage.input_tokens, 100);
        assert_eq!(response.usage.output_tokens, 50);

        let seen = transport
            .seen
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].path, "/chat/completions");
        assert_eq!(seen[0].body, seen[1].body);
        assert_eq!(
            seen[0].header("authorization"),
            Some("Bearer relay-api-key")
        );
        assert_eq!(seen[1].header(HEADER_VERSION), Some("0.1"));
        assert_eq!(
            seen[1].header(HEADER_REQUEST_ID),
            Some("11111111-1111-4111-8111-111111111111")
        );
        let authorization = decode_authorization(
            seen[1]
                .header(HEADER_AUTHORIZATION)
                .expect("signed retry authorization"),
        )
        .expect("authorization decodes");
        assert_eq!(authorization.payer, "acct_payer");
        assert_eq!(authorization.payee, "ait1qrelay");
        assert_eq!(authorization.amount_micros, AmountMicros(50_000));
        assert_eq!(authorization.scheme, Scheme::Hold);
    }

    #[tokio::test]
    async fn funded_high_level_model_sends_once_without_direct_payment() {
        let transport = paid_transport(PaidFlow::Funded);
        let model = paid_model(transport.clone());
        let request = model.completion_request("hello").build();
        model.completion(request).await.expect("funded completion");
        assert_eq!(transport.seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn second_402_is_not_retried_again() {
        let transport = paid_transport(PaidFlow::ChallengeTwice);
        let model = paid_model(transport.clone());
        let request = model.completion_request("hello").build();
        assert!(model.completion(request).await.is_err());
        assert_eq!(transport.seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn paid_stream_is_incremental_and_ends_after_durable_evidence() {
        let transport = StreamingRelay::new(StreamingFlow::Success);
        let model = paid_streaming_model(transport.clone());
        let request = model.completion_request("hello").build();
        let mut response = model.stream(request).await.expect("stream opens");

        let mut text = Vec::new();
        let mut usage = None;
        while let Some(item) = response.next().await {
            match item.expect("stream item") {
                StreamedAssistantContent::Text(chunk) => text.push(chunk.text),
                StreamedAssistantContent::Final(final_response) => {
                    usage = Some(final_response.usage)
                }
                _ => {}
            }
        }

        assert_eq!(text, ["hel", "lo"]);
        let usage = usage.expect("terminal usage");
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.total_tokens - usage.prompt_tokens, 50);

        let stream_seen = transport
            .stream_seen
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        assert_eq!(stream_seen.len(), 2, "one challenge plus one signed retry");
        assert_eq!(stream_seen[0].body, stream_seen[1].body);
        assert!(stream_seen[0].header(HEADER_AUTHORIZATION).is_none());
        assert!(stream_seen[1].header(HEADER_AUTHORIZATION).is_some());
        drop(stream_seen);
        assert_eq!(transport.lookup_seen.lock().unwrap().len(), 1);

        let evidence = model.drain_payment_evidence();
        assert_eq!(evidence.len(), 1);
        assert_eq!(
            evidence[0].request_id.as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );
        assert_eq!(evidence[0].payer, "acct_payer");
        assert_eq!(
            evidence[0].owner_principal.as_deref(),
            Some("acct_api_owner")
        );
        assert_eq!(
            evidence[0].receipt.as_ref().map(|receipt| receipt.seq),
            Some(42)
        );
        assert!(model.drain_payment_evidence().is_empty());
    }

    #[tokio::test]
    async fn paid_stream_missing_usage_fails_without_resubmitting_or_looking_up_proof() {
        let transport = StreamingRelay::new(StreamingFlow::MissingUsage);
        let model = paid_streaming_model(transport.clone());
        let request = model.completion_request("hello").build();
        let mut response = model.stream(request).await.expect("stream opens");
        let mut failed = false;
        while let Some(item) = response.next().await {
            if item.is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed, "missing terminal usage must fail closed");
        assert_eq!(transport.stream_seen.lock().unwrap().len(), 2);
        assert!(transport.lookup_seen.lock().unwrap().is_empty());
        assert!(model.drain_payment_evidence().is_empty());
    }

    #[tokio::test]
    async fn paid_stream_disconnect_fails_without_resubmitting() {
        let transport = StreamingRelay::new(StreamingFlow::BodyError);
        let model = paid_streaming_model(transport.clone());
        let request = model.completion_request("hello").build();
        let mut response = model.stream(request).await.expect("stream opens");
        let mut failed = false;
        while let Some(item) = response.next().await {
            if item.is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed, "transport disconnect must reach the caller");
        assert_eq!(transport.stream_seen.lock().unwrap().len(), 2);
        assert!(transport.lookup_seen.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn paid_stream_application_error_is_sticky_and_never_looks_up_success() {
        let transport = StreamingRelay::new(StreamingFlow::ApplicationError);
        let model = paid_streaming_model(transport.clone());
        let request = model.completion_request("hello").build();
        let mut response = model.stream(request).await.expect("stream opens");
        let mut failed = false;
        while let Some(item) = response.next().await {
            if item.is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed, "application-level error must remain terminal");
        assert_eq!(transport.stream_seen.lock().unwrap().len(), 2);
        assert!(transport.lookup_seen.lock().unwrap().is_empty());
        assert!(model.drain_payment_evidence().is_empty());
    }

    #[tokio::test]
    async fn paid_stream_without_durable_receipt_fails_after_terminal_usage() {
        let transport = StreamingRelay::new(StreamingFlow::MissingEvidence);
        let model = paid_streaming_model(transport.clone());
        let request = model.completion_request("hello").build();
        let mut response = model.stream(request).await.expect("stream opens");
        let mut failed = false;
        while let Some(item) = response.next().await {
            if item.is_err() {
                failed = true;
                break;
            }
        }
        assert!(failed, "404 settlement evidence must fail the paid stream");
        assert_eq!(transport.stream_seen.lock().unwrap().len(), 2);
        assert_eq!(transport.lookup_seen.lock().unwrap().len(), 1);
        assert!(model.drain_payment_evidence().is_empty());
        assert_eq!(
            model.pending_payment_request_ids(),
            ["11111111-1111-4111-8111-111111111111"]
        );

        transport.evidence_available.store(true, Ordering::SeqCst);
        let recovered = model
            .reconcile_payment_evidence()
            .await
            .expect("explicit evidence recovery");
        assert_eq!(recovered.len(), 1);
        assert!(model.pending_payment_request_ids().is_empty());
        assert_eq!(model.drain_payment_evidence().len(), 1);
    }

    #[tokio::test]
    async fn dropping_paid_stream_reconciles_verified_evidence_in_background() {
        let transport = StreamingRelay::new(StreamingFlow::Success);
        let model = paid_streaming_model(transport.clone());
        let request = model.completion_request("hello").build();
        let mut response = model.stream(request).await.expect("stream opens");

        assert!(response.next().await.is_some(), "first chunk arrives");
        drop(response);

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if model.pending_payment_request_ids().is_empty()
                    && !transport.lookup_seen.lock().unwrap().is_empty()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("drop-time proof reconciliation completes");

        let evidence = model.drain_payment_evidence();
        assert_eq!(evidence.len(), 1);
        assert!(evidence[0].attestation.is_some());
        assert_eq!(evidence[0].verified_at_ms, Some(1_699_999_999_999));
        assert_eq!(transport.stream_seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn tampered_relay_projection_cannot_become_payment_evidence() {
        let transport = StreamingRelay::new(StreamingFlow::TamperedEvidence);
        let model = paid_streaming_model(transport);
        let request = model.completion_request("hello").build();
        let mut response = model.stream(request).await.expect("stream opens");
        let mut failed = false;
        while let Some(item) = response.next().await {
            if item.is_err() {
                failed = true;
                break;
            }
        }

        assert!(failed, "receipt/attestation mismatch must fail closed");
        assert!(model.drain_payment_evidence().is_empty());
        assert_eq!(model.pending_payment_request_ids().len(), 1);
    }

    #[tokio::test]
    async fn multipart_body_is_replayed_identically_through_one_paid_retry() {
        let transport = paid_transport(PaidFlow::ChallengeThenOk);
        let policy = paid_config()
            .payment_policy("ait1qrelay", AmountMicros(100_000))
            .expect("policy builds");
        let http = AitpRigHttpClient::with_transport(
            "http://relay.test/openai/v1",
            "acct_payer",
            Arc::new(Ed25519Signer::from_seed("payer-key", &[7; 32])),
            policy,
            mint_verifier(),
            transport.clone(),
        );
        let form = MultipartForm::new()
            .boundary("aitp-test-boundary")
            .text("model", "openai::gpt-4o")
            .text("prompt", "hello");
        let request = Request::builder()
            .method("POST")
            .uri("http://relay.test/openai/v1/chat/completions")
            .header("authorization", "Bearer relay-api-key")
            .body(form)
            .expect("multipart request");

        let response = HttpClientExt::send_multipart::<Bytes>(&http, request)
            .await
            .expect("multipart paid retry");
        assert_eq!(response.status().as_u16(), 200);

        let seen = transport.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].body, seen[1].body);
        assert!(String::from_utf8_lossy(&seen[0].body).contains("aitp-test-boundary"));
        assert_eq!(
            seen[1].header("content-type"),
            Some("multipart/form-data; boundary=aitp-test-boundary")
        );
        assert!(seen[0].header(HEADER_AUTHORIZATION).is_none());
        assert!(seen[1].header(HEADER_AUTHORIZATION).is_some());
    }

    #[tokio::test]
    async fn paid_stream_second_402_is_terminal() {
        let transport = StreamingRelay::new(StreamingFlow::ChallengeTwice);
        let model = paid_streaming_model(transport.clone());
        let request = model.completion_request("hello").build();
        let mut response = model.stream(request).await.expect("lazy stream builds");
        assert!(response.next().await.is_some_and(|item| item.is_err()));
        assert_eq!(transport.stream_seen.lock().unwrap().len(), 2);
        assert!(transport.lookup_seen.lock().unwrap().is_empty());
    }

    #[test]
    fn payment_policy_is_bound_to_payee_model_path_and_amount() {
        let policy = paid_config()
            .payment_policy("ait1qrelay", AmountMicros(100_000))
            .expect("policy builds");
        assert_eq!(policy.expected_payee, "ait1qrelay");
        assert_eq!(policy.max_amount_micros, AmountMicros(100_000));
        assert_eq!(
            policy.resource_prefix,
            "POST /openai/v1/chat/completions#openrouter/openai/gpt-4o@"
        );
    }
}
