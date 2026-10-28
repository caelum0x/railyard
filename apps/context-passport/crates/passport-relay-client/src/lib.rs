//! Real HTTP client to the Relay gateway, implementing `RelayConditioningSink`.
//!
//! Relay (a TensorZero fork) performs the provider fan-out; the Passport supplies
//! the per-provider conditioning. This client submits a resolved
//! `RelayGenerationRequest` and returns the produced asset reference.
//!
//! # Wire contract
//!
//! The gateway exposes the passport-conditioned surface at
//! `POST /v1/passport/generate`, which is a native TensorZero inference route
//! (it delegates to the same handler as `/inference` and is wrapped by Relay's
//! success-only billing/AITP settlement middleware). This client therefore
//! translates the resolved [`RelayGenerationRequest`] into a native inference
//! request — function `passport_<modality>` plus the assembled conditioning as
//! the input — and reads the produced inference back as the generated asset
//! reference. The signed settlement evidence is transported out-of-band in the
//! `AITP-Receipt` / `AITP-Request-ID` response headers. The transport, auth,
//! contract mapping, and error mapping are real; tests fake the sink via
//! `FakeRelay`.

use async_trait::async_trait;
use mint_protocol::{decode_receipt, HEADER_RECEIPT};
use serde_json::{json, Value};

use passport_core::{
    PassportError, PaymentEvidence, RelayConditioningSink, RelayGenerationRequest,
    RelayGenerationResult, SettlementReceiptProof,
};

const HEADER_REQUEST_ID: &str = "AITP-Request-ID";

#[derive(Clone)]
pub struct RelayClient {
    http: reqwest::Client,
    base: String,
    api_key: String,
    /// Path the conditioned-generation request is POSTed to.
    path: String,
}

impl RelayClient {
    pub fn new(base: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            path: "/v1/passport/generate".to_string(),
        }
    }

    /// Override the Relay generation path (defaults to `/v1/passport/generate`).
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }
}

#[async_trait]
impl RelayConditioningSink for RelayClient {
    async fn generate(
        &self,
        request: &RelayGenerationRequest,
    ) -> Result<RelayGenerationResult, PassportError> {
        let inference_request = build_inference_request(request);
        let response = self
            .http
            .post(format!("{}{}", self.base, self.path))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .json(&inference_request)
            .send()
            .await
            .map_err(|e| PassportError::Relay(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            let lowered = body.to_ascii_lowercase();
            if lowered.contains("unknown function") || lowered.contains("unknownfunction") {
                return Err(PassportError::Relay(format!(
                    "Relay has no `passport_{}` function configured for {} generation; \
                     generate this modality with your asset provider and record it via \
                     POST /v1/passports/:id/attest",
                    request.modality, request.modality
                )));
            }
            if lowered.contains("unknown variant") || lowered.contains("unknownvariant") {
                return Err(PassportError::Relay(format!(
                    "Relay has no `{}` provider variant for passport_{}",
                    provider_variant(&request.provider),
                    request.modality
                )));
            }
            return Err(PassportError::Relay(format!(
                "relay returned {status}: {body}"
            )));
        }

        // Signed settlement evidence is transported in the response headers by
        // the gateway's billing middleware; capture it BEFORE consuming the body.
        let payment_evidence = payment_evidence(response.headers(), &request.principal)?;
        let body = response
            .json::<Value>()
            .await
            .map_err(|e| PassportError::Relay(format!("decoding relay response: {e}")))?;
        relay_result(&body, Some(payment_evidence))
    }
}

/// Translate a resolved [`RelayGenerationRequest`] into a native TensorZero
/// inference request the gateway accepts at `/v1/passport/generate`.
///
/// The function is selected per modality (`passport_<modality>`) and the
/// provider-neutral conditioning payload is assembled into the model input. The
/// owning passport identity travels as `tags` so the produced inference is
/// attributable in Relay's observability store (which is what `output_ref`
/// later references).
fn build_inference_request(request: &RelayGenerationRequest) -> Value {
    json!({
        "function_name": format!("passport_{}", request.modality),
        "variant_name": provider_variant(&request.provider),
        "input": {
            // The locked identity conditions the model as the system turn; the
            // caller's instruction is the user turn.
            "system": assemble_conditioning_prompt(request),
            "messages": [
                { "role": "user", "content": user_instruction(request) }
            ]
        },
        "tags": {
            "passport_id": request.passport_id,
            "passport_version": request.version.to_string(),
            "passport_provider": request.provider,
            "passport_modality": request.modality,
            "passport_principal": request.principal,
        },
        "stream": false,
    })
}

/// Map the passport's `provider` to the Relay `passport_text` variant that runs
/// it. Relay names one variant per supported text provider (the lowercased
/// provider id — see `config/tensorzero.toml`), and pinning `variant_name`
/// makes the gateway route to exactly that model instead of the single
/// hardcoded default. The provider is lowercased to match Relay's variant
/// naming (the resolver already lowercases providers for capability lookup). An
/// unsupported provider names a variant Relay does not define, so the gateway
/// rejects it with `UnknownVariant` and the generation fails closed rather than
/// silently running on the wrong model.
fn provider_variant(provider: &str) -> String {
    provider.trim().to_ascii_lowercase()
}

/// The user turn: the caller's instruction, or a neutral self-introduction when
/// no prompt was given (so a bare call still yields visible, on-identity output).
fn user_instruction(request: &RelayGenerationRequest) -> String {
    match request.prompt.as_deref().map(str::trim) {
        Some(prompt) if !prompt.is_empty() => prompt.to_string(),
        _ => format!(
            "Write a short {} sample that showcases this identity.",
            request.modality
        ),
    }
}

/// Assemble the resolved conditioning payload into a single instruction string.
///
/// `payload` is the serialized `ConditioningPayload` (opaque to this crate), so
/// every field is read defensively: a missing or differently-shaped field is
/// skipped rather than failing the generation.
fn assemble_conditioning_prompt(request: &RelayGenerationRequest) -> String {
    let payload = &request.payload;
    let mut sections: Vec<String> = Vec::new();

    let style = payload
        .get("style_prompt")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    if let Some(style) = style {
        sections.push(format!("Style: {style}"));
    }
    let directives: Vec<&str> = payload
        .get("text_directives")
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if !directives.is_empty() {
        sections.push(format!("Directives:\n- {}", directives.join("\n- ")));
    }
    let negative = payload
        .get("negative_prompt")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty());
    if let Some(negative) = negative {
        sections.push(format!("Avoid: {negative}"));
    }

    if sections.is_empty() {
        // The resolver returned no usable directives; still emit a well-formed,
        // passport-scoped instruction so the request is a valid inference.
        return format!(
            "Generate {modality} content as passport {passport} (version {version}).",
            modality = request.modality,
            passport = request.passport_id,
            version = request.version,
        );
    }

    format!(
        "Generate {modality} content conditioned as passport {passport} (version {version}).\n\n{body}",
        modality = request.modality,
        passport = request.passport_id,
        version = request.version,
        body = sections.join("\n\n"),
    )
}

/// Build a [`RelayGenerationResult`] from a native inference response body.
///
/// The generated asset is the stored inference itself, referenced by its
/// `inference_id`. For chat functions the produced text blocks are returned as
/// `output_text` and hard-bound into provenance via `asset_hash`
/// (`sha256:<hex>` of the exact text).
fn relay_result(
    body: &Value,
    payment_evidence: Option<PaymentEvidence>,
) -> Result<RelayGenerationResult, PassportError> {
    let inference_id = body
        .get("inference_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| {
            PassportError::Relay("relay inference response missing inference_id".into())
        })?;
    let output_text = body
        .get("content")
        .and_then(Value::as_array)
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("")
        })
        .filter(|text| !text.is_empty());
    let asset_hash = output_text.as_deref().map(|text| {
        use sha2::{Digest, Sha256};
        format!("sha256:{}", hex::encode(Sha256::digest(text.as_bytes())))
    });
    Ok(RelayGenerationResult {
        output_ref: format!("relay-inference:{inference_id}"),
        asset_hash,
        payment_evidence,
        output_text,
    })
}

fn payment_evidence(
    headers: &reqwest::header::HeaderMap,
    principal: &str,
) -> Result<PaymentEvidence, PassportError> {
    let encoded_receipt = headers
        .get(HEADER_RECEIPT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| PassportError::Relay("Relay response missing AITP-Receipt".into()))?;
    let request_id = headers
        .get(HEADER_REQUEST_ID)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| PassportError::Relay("Relay response missing AITP-Request-ID".into()))?;
    let receipt = decode_receipt(&encoded_receipt)
        .map_err(|e| PassportError::Relay(format!("decoding Relay receipt: {e}")))?;
    if !matches!(receipt.op.as_str(), "hold.capture" | "transfer") {
        return Err(PassportError::Relay(format!(
            "Relay generation returned non-settling {} receipt",
            receipt.op
        )));
    }
    Ok(PaymentEvidence {
        request_id,
        payer_account: principal.to_string(),
        receipt: SettlementReceiptProof {
            sequence: receipt.seq,
            operation: receipt.op,
            operation_hash: receipt.op_hash,
            amount_micros: receipt.amount_micros,
            receipt_hash: receipt.hash,
            signature: receipt.sig,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    const RECEIPT: &str = "eyJzZXEiOjQyLCJvcCI6ImhvbGQuY2FwdHVyZSIsIm9wX2hhc2giOiJhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhIiwiYW1vdW50X21pY3JvcyI6OTkwMDAwLCJiYWxhbmNlX2FmdGVyX21pY3JvcyI6NzAxMDAwMCwicHJldl9oYXNoIjoiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYiIsImhhc2giOiJjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjIiwic2lnIjoiZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWUifQ";

    #[test]
    fn decodes_principal_and_request_correlated_signed_evidence() {
        let mut headers = HeaderMap::new();
        headers.insert(HEADER_RECEIPT, HeaderValue::from_static(RECEIPT));
        headers.insert(HEADER_REQUEST_ID, HeaderValue::from_static("req_1"));
        let evidence = payment_evidence(&headers, "acct_owner").unwrap();
        assert_eq!(evidence.request_id, "req_1");
        assert_eq!(evidence.payer_account, "acct_owner");
        assert_eq!(evidence.receipt.operation, "hold.capture");
        assert_eq!(evidence.receipt.sequence, 42);
        assert!(!evidence.receipt.signature.is_empty());
    }

    #[test]
    fn missing_receipt_fails_closed() {
        let mut headers = HeaderMap::new();
        headers.insert(HEADER_REQUEST_ID, HeaderValue::from_static("req_1"));
        let error = payment_evidence(&headers, "acct_owner").unwrap_err();
        assert!(matches!(error, PassportError::Relay(message) if message.contains("AITP-Receipt")));
    }

    fn sample_request() -> RelayGenerationRequest {
        RelayGenerationRequest {
            principal: "acct_owner".into(),
            passport_id: "psp_1".into(),
            version: 3,
            provider: "openai".into(),
            modality: "text".into(),
            payload: json!({
                "style_prompt": "warm, punchy, brand voice",
                "text_directives": ["hook in 3s", "call to action"],
                "negative_prompt": "no jargon",
            }),
            prompt: Some("Announce the spring launch".into()),
        }
    }

    #[test]
    fn maps_request_to_modality_function_with_conditioning_input() {
        let request = build_inference_request(&sample_request());
        assert_eq!(request["function_name"], "passport_text");
        // The passport's provider pins the routed variant so the gateway runs
        // that provider's model instead of one hardcoded default.
        assert_eq!(request["variant_name"], "openai");
        assert_eq!(request["stream"], false);
        assert_eq!(request["tags"]["passport_id"], "psp_1");
        assert_eq!(request["tags"]["passport_version"], "3");
        assert_eq!(request["tags"]["passport_principal"], "acct_owner");
        assert_eq!(
            request["input"]["messages"][0]["content"],
            "Announce the spring launch"
        );
        let content = request["input"]["system"]
            .as_str()
            .expect("assembled conditioning");
        assert!(content.contains("warm, punchy, brand voice"));
        assert!(content.contains("hook in 3s"));
        assert!(content.contains("call to action"));
        assert!(content.contains("Avoid: no jargon"));
    }

    #[test]
    fn provider_selects_matching_routed_variant() {
        let mut request = sample_request();
        request.provider = "anthropic".into();
        let inference = build_inference_request(&request);
        assert_eq!(inference["variant_name"], "anthropic");
        // Same modality function, different pinned provider variant.
        assert_eq!(inference["function_name"], "passport_text");
    }

    #[test]
    fn provider_variant_is_normalized_to_relay_naming() {
        // Relay names variants by the lowercased provider id; casing/padding in
        // the passport's provider field must still route to the right variant.
        assert_eq!(provider_variant("OpenAI"), "openai");
        assert_eq!(provider_variant(" Anthropic "), "anthropic");
    }

    #[test]
    fn empty_conditioning_still_yields_valid_instruction() {
        let mut request = sample_request();
        request.payload = json!({});
        let content = assemble_conditioning_prompt(&request);
        assert!(content.contains("passport psp_1"));
        assert!(content.contains("text"));
    }

    #[test]
    fn result_references_inference_by_id() {
        let evidence = PaymentEvidence {
            request_id: "req_1".into(),
            payer_account: "acct_owner".into(),
            receipt: SettlementReceiptProof {
                sequence: 1,
                operation: "hold.capture".into(),
                operation_hash: "h".into(),
                amount_micros: 10,
                receipt_hash: "rh".into(),
                signature: "sig".into(),
            },
        };
        let body = json!({
            "inference_id": "0193f0a0-0000-7000-8000-000000000000",
            "usage": { "input_tokens": 10, "output_tokens": 20 },
        });
        let result = relay_result(&body, Some(evidence)).unwrap();
        assert_eq!(
            result.output_ref,
            "relay-inference:0193f0a0-0000-7000-8000-000000000000"
        );
        assert!(result.asset_hash.is_none());
        assert!(result.payment_evidence.is_some());
    }

    #[test]
    fn missing_inference_id_fails_closed() {
        let error = relay_result(&json!({ "usage": {} }), None).unwrap_err();
        assert!(matches!(error, PassportError::Relay(message) if message.contains("inference_id")));
    }

    /// Regression test for a real deploy bug (2026-10-28): `RelayClient::generate`
    /// sent `X-API-Key` instead of `Authorization: Bearer <key>`, so every real
    /// call to a live Relay gateway failed `401 Unauthorized` before this fix —
    /// invisible to every other test here because they all exercise
    /// `build_inference_request`/`payment_evidence` directly, never the real
    /// `reqwest` call this trait impl makes. Spins up a real local HTTP server
    /// (no fakes) and asserts the actual outbound request header, end to end
    /// through the real `generate()` call.
    #[tokio::test]
    async fn generate_sends_bearer_authorization_not_x_api_key() {
        use axum::extract::Json as AxumJson;
        use axum::http::HeaderMap as AxumHeaderMap;
        use axum::response::IntoResponse;
        use axum::routing::post;
        use axum::Router;
        use std::sync::{Arc, Mutex};

        let seen_auth = Arc::new(Mutex::new(None::<String>));
        let captured = seen_auth.clone();
        let app = Router::new().route(
            "/v1/passport/generate",
            post(move |headers: AxumHeaderMap, AxumJson(_body): AxumJson<Value>| {
                let captured = captured.clone();
                async move {
                    *captured.lock().unwrap() = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .map(str::to_string);
                    let mut response = axum::Json(json!({
                        "inference_id": "0193f0a0-0000-7000-8000-000000000001",
                        "usage": { "input_tokens": 1, "output_tokens": 1 },
                    }))
                    .into_response();
                    response
                        .headers_mut()
                        .insert(HEADER_RECEIPT, HeaderValue::from_static(RECEIPT));
                    response
                        .headers_mut()
                        .insert(HEADER_REQUEST_ID, HeaderValue::from_static("req_live_1"));
                    response
                }
            }),
        );
        let listener = match tokio::net::TcpListener::bind("127.0.0.1:0").await {
            Ok(listener) => listener,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => return,
            Err(error) => panic!("bind test server: {error}"),
        };
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let client = RelayClient::new(base, "sk-t0-real-looking-key");
        let result = client.generate(&sample_request()).await.unwrap();

        assert_eq!(
            result.output_ref,
            "relay-inference:0193f0a0-0000-7000-8000-000000000001"
        );
        assert!(result.payment_evidence.is_some());
        assert_eq!(
            seen_auth.lock().unwrap().as_deref(),
            Some("Bearer sk-t0-real-looking-key"),
            "RelayClient must send Authorization: Bearer <key>, the header Relay's \
             gateway actually checks — not X-API-Key, which Relay silently ignores \
             and answers 401 to"
        );
    }
}
