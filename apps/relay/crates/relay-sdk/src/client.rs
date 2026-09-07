use std::sync::Arc;

use futures::StreamExt;
use http::HeaderMap;
use http::header::{ACCEPT, AUTHORIZATION, CONTENT_TYPE, HeaderName, HeaderValue};
use relay_catalog::{CatalogSnapshot, PriceQuote, PriceQuoteRequest};
use relay_contract::headers::{
    AITP_AUTHORIZATION_HEADER, AITP_CHALLENGE_HEADER, AITP_RECEIPT_HEADER, AITP_REQUEST_ID_HEADER,
    AITP_VERSION_HEADER, RELAY_BILLING_OVERAGE_HEADER,
};
use relay_contract::{
    AitpChallenge, ApiErrorEnvelope, DataEnvelope, GatewayHealth, GatewayStatus, PaymentRequired,
    SettledReplay, SettlementEvidence, Usage,
};
use relay_cost_feed::{CostEventListQuery, CostEventPage, PendingCostEvent};
use serde::Serialize;
use serde::de::DeserializeOwned;
use uuid::Uuid;

use crate::config::RelayClientConfig;
use crate::error::{ApiFailure, RelaySdkError, Result};
use crate::http::{HttpRequest, HttpResponse, HttpTransport, ReqwestTransport};
use crate::multipart::MultipartForm;
use crate::recovery::AbandonedStreamHandler;
use crate::request::{RelayEndpoint, RequestOptions};
use crate::response::{RelayResponse, RelayResponseMetadata, RelayStream};

#[derive(Clone)]
pub struct RelayClient {
    config: RelayClientConfig,
    transport: Arc<dyn HttpTransport>,
    abandoned_streams: Option<Arc<dyn AbandonedStreamHandler>>,
}

impl RelayClient {
    pub fn new(config: RelayClientConfig) -> Result<Self> {
        let transport = ReqwestTransport::new(
            config.connect_timeout,
            config.read_timeout,
            &config.user_agent,
        )?;
        Ok(Self::with_transport(config, Arc::new(transport)))
    }

    pub fn with_transport(config: RelayClientConfig, transport: Arc<dyn HttpTransport>) -> Self {
        Self {
            config,
            transport,
            abandoned_streams: None,
        }
    }

    pub fn with_abandoned_stream_handler(
        mut self,
        handler: Arc<dyn AbandonedStreamHandler>,
    ) -> Self {
        self.abandoned_streams = Some(handler);
        self
    }

    pub async fn status(&self) -> Result<RelayResponse<GatewayStatus>> {
        self.json(
            RelayEndpoint::Status,
            serde_json::Value::Null,
            RequestOptions::default(),
        )
        .await
    }

    pub async fn health(&self) -> Result<RelayResponse<GatewayHealth>> {
        let response = self
            .send(
                RelayEndpoint::Health,
                Vec::new(),
                Some("application/json"),
                RequestOptions::default(),
                false,
            )
            .await?;
        // TensorZero deliberately returns the dependency snapshot with 503 when
        // any required service is unhealthy. Preserve that typed diagnostic for
        // operators instead of reducing it to a generic API failure.
        self.decode_json_accepting(response, |status| status == 503)
            .await
    }

    pub async fn catalog(&self) -> Result<RelayResponse<CatalogSnapshot>> {
        let response: RelayResponse<DataEnvelope<CatalogSnapshot>> = self
            .json(
                RelayEndpoint::Catalog,
                serde_json::Value::Null,
                RequestOptions::default(),
            )
            .await?;
        Ok(RelayResponse {
            data: response.data.data,
            usage: response.usage,
            metadata: response.metadata,
        })
    }

    pub async fn quote(&self, request: PriceQuoteRequest) -> Result<RelayResponse<PriceQuote>> {
        let response: RelayResponse<DataEnvelope<PriceQuote>> = self
            .json(RelayEndpoint::Quote, request, RequestOptions::default())
            .await?;
        Ok(RelayResponse {
            data: response.data.data,
            usage: response.usage,
            metadata: response.metadata,
        })
    }

    pub async fn cost_event(&self, event_id: Uuid) -> Result<RelayResponse<PendingCostEvent>> {
        let response: RelayResponse<DataEnvelope<PendingCostEvent>> = self
            .json(
                RelayEndpoint::CostEvent(event_id),
                serde_json::Value::Null,
                RequestOptions::default(),
            )
            .await?;
        Ok(RelayResponse {
            data: response.data.data,
            usage: response.usage,
            metadata: response.metadata,
        })
    }

    pub async fn cost_events(
        &self,
        query: CostEventListQuery,
    ) -> Result<RelayResponse<CostEventPage>> {
        let query = query
            .validate()
            .map_err(|error| RelaySdkError::InvalidRequest(error.to_string()))?;
        let response: RelayResponse<DataEnvelope<CostEventPage>> = self
            .json(
                RelayEndpoint::CostEvents(query),
                serde_json::Value::Null,
                RequestOptions::default(),
            )
            .await?;
        Ok(RelayResponse {
            data: response.data.data,
            usage: response.usage,
            metadata: response.metadata,
        })
    }

    pub async fn replay_cost_event(
        &self,
        event_id: Uuid,
    ) -> Result<RelayResponse<PendingCostEvent>> {
        let response: RelayResponse<DataEnvelope<PendingCostEvent>> = self
            .json(
                RelayEndpoint::ReplayCostEvent(event_id),
                serde_json::json!({}),
                RequestOptions::default(),
            )
            .await?;
        Ok(RelayResponse {
            data: response.data.data,
            usage: response.usage,
            metadata: response.metadata,
        })
    }

    pub async fn chat_completions<T: DeserializeOwned>(
        &self,
        body: impl Serialize,
        options: RequestOptions,
    ) -> Result<RelayResponse<T>> {
        self.json(RelayEndpoint::OpenAiChatCompletions, body, options)
            .await
    }

    pub async fn chat_completions_stream(
        &self,
        body: impl Serialize,
        options: RequestOptions,
    ) -> Result<RelayStream> {
        self.json_stream(RelayEndpoint::OpenAiChatCompletions, body, options)
            .await
    }

    pub async fn embeddings<T: DeserializeOwned>(
        &self,
        body: impl Serialize,
        options: RequestOptions,
    ) -> Result<RelayResponse<T>> {
        self.json(RelayEndpoint::OpenAiEmbeddings, body, options)
            .await
    }

    pub async fn inference<T: DeserializeOwned>(
        &self,
        body: impl Serialize,
        options: RequestOptions,
    ) -> Result<RelayResponse<T>> {
        self.json(RelayEndpoint::NativeInference, body, options)
            .await
    }

    pub async fn inference_stream(
        &self,
        body: impl Serialize,
        options: RequestOptions,
    ) -> Result<RelayStream> {
        self.json_stream(RelayEndpoint::NativeInference, body, options)
            .await
    }

    pub async fn batch_inference<T: DeserializeOwned>(
        &self,
        body: impl Serialize,
        options: RequestOptions,
    ) -> Result<RelayResponse<T>> {
        self.json(RelayEndpoint::NativeBatchInference, body, options)
            .await
    }

    pub async fn batch_inference_status<T: DeserializeOwned>(
        &self,
        batch_id: Uuid,
    ) -> Result<RelayResponse<T>> {
        self.json(
            RelayEndpoint::BatchInferenceStatus(batch_id),
            serde_json::Value::Null,
            RequestOptions::default(),
        )
        .await
    }

    pub async fn settlement_evidence(
        &self,
        request_id: Uuid,
    ) -> Result<RelayResponse<SettlementEvidence>> {
        let response = self.settlement_evidence_unverified(request_id).await?;
        let Some(validator) = &self.config.evidence_validator else {
            return Err(RelaySdkError::UnverifiedSettlementEvidence(Box::new(
                response.data,
            )));
        };
        validator
            .validate(&response.data)
            .map_err(|error| RelaySdkError::InvalidSettlementEvidence(error.to_string()))?;
        Ok(response)
    }

    /// Retrieve Relay's authenticated projection without treating it as Mint
    /// proof. Recovery workers use this and apply their explicitly supplied
    /// validator before transitioning to `Verified`.
    pub async fn settlement_evidence_unverified(
        &self,
        request_id: Uuid,
    ) -> Result<RelayResponse<SettlementEvidence>> {
        let response: RelayResponse<DataEnvelope<SettlementEvidence>> = self
            .json(
                RelayEndpoint::SettlementEvidence(request_id),
                serde_json::Value::Null,
                RequestOptions::default(),
            )
            .await?;
        Ok(RelayResponse {
            data: response.data.data,
            usage: response.usage,
            metadata: response.metadata,
        })
    }

    pub async fn json<T: DeserializeOwned>(
        &self,
        endpoint: RelayEndpoint,
        body: impl Serialize,
        options: RequestOptions,
    ) -> Result<RelayResponse<T>> {
        let mut value = serde_json::to_value(body)
            .map_err(|error| RelaySdkError::InvalidRequest(error.to_string()))?;
        if options.dry_run {
            let object = value.as_object_mut().ok_or_else(|| {
                RelaySdkError::InvalidRequest("dry-run JSON body must be an object".into())
            })?;
            object.insert("tensorzero::dryrun".into(), true.into());
        }
        let body = if endpoint.method() == http::Method::GET {
            Vec::new()
        } else {
            serde_json::to_vec(&value)
                .map_err(|error| RelaySdkError::InvalidRequest(error.to_string()))?
        };
        let response = self
            .send(endpoint, body, Some("application/json"), options, false)
            .await?;
        self.decode_json(response).await
    }

    pub async fn json_stream(
        &self,
        endpoint: RelayEndpoint,
        body: impl Serialize,
        options: RequestOptions,
    ) -> Result<RelayStream> {
        let mut value = serde_json::to_value(body)
            .map_err(|error| RelaySdkError::InvalidRequest(error.to_string()))?;
        if options.dry_run {
            let object = value.as_object_mut().ok_or_else(|| {
                RelaySdkError::InvalidRequest("dry-run JSON body must be an object".into())
            })?;
            object.insert("tensorzero::dryrun".into(), true.into());
        }
        let body = serde_json::to_vec(&value)
            .map_err(|error| RelaySdkError::InvalidRequest(error.to_string()))?;
        let response = self
            .send(endpoint, body, Some("application/json"), options, true)
            .await?;
        let metadata = metadata(response.status, &response.headers);
        if !(200..300).contains(&response.status) {
            return Err(self.decode_error(response).await);
        }
        Ok(RelayStream::new(
            metadata,
            response.body,
            self.config.max_sse_event_bytes,
            self.abandoned_streams.clone(),
        ))
    }

    pub async fn multipart<T: DeserializeOwned>(
        &self,
        endpoint: RelayEndpoint,
        form: MultipartForm,
        options: RequestOptions,
    ) -> Result<RelayResponse<T>> {
        if endpoint.method() == http::Method::GET {
            return Err(RelaySdkError::InvalidRequest(
                "multipart is not valid for a GET endpoint".into(),
            ));
        }
        let (content_type, body) = form.encode();
        let response = self
            .send(endpoint, body, Some(&content_type), options, false)
            .await?;
        self.decode_json(response).await
    }

    async fn send(
        &self,
        endpoint: RelayEndpoint,
        body: Vec<u8>,
        content_type: Option<&str>,
        options: RequestOptions,
        streaming: bool,
    ) -> Result<HttpResponse> {
        validate_options(&options)?;
        let path = endpoint.path();
        validate_relative_path(&path)?;
        let url = self
            .config
            .base_url
            .join(&path)
            .map_err(|error| RelaySdkError::InvalidRequest(error.to_string()))?;
        let mut headers = HeaderMap::new();
        headers.insert(
            ACCEPT,
            HeaderValue::from_static(if streaming {
                "text/event-stream"
            } else {
                "application/json"
            }),
        );
        if let Some(content_type) = content_type {
            insert_header(&mut headers, CONTENT_TYPE, content_type)?;
        }
        if let Some(value) = self.config.credential.authorization_value() {
            insert_header(&mut headers, AUTHORIZATION, &value)?;
        }
        if let Some(value) = options.aitp_authorization.as_deref() {
            insert_static_header(&mut headers, AITP_AUTHORIZATION_HEADER, value)?;
        }
        if let Some(request_id) = options.aitp_request_id {
            insert_static_header(
                &mut headers,
                AITP_REQUEST_ID_HEADER,
                &request_id.to_string(),
            )?;
        }
        if let Some(idempotency_key) = options.idempotency_key.as_deref() {
            insert_static_header(&mut headers, "idempotency-key", idempotency_key)?;
        }
        self.transport
            .send(HttpRequest {
                method: endpoint.method(),
                url,
                headers,
                body,
                timeout: if streaming {
                    self.config.stream_timeout
                } else {
                    self.config.request_timeout
                },
            })
            .await
    }

    async fn decode_json<T: DeserializeOwned>(
        &self,
        response: HttpResponse,
    ) -> Result<RelayResponse<T>> {
        self.decode_json_accepting(response, |_| false).await
    }

    async fn decode_json_accepting<T: DeserializeOwned>(
        &self,
        response: HttpResponse,
        accept_non_success: impl FnOnce(u16) -> bool,
    ) -> Result<RelayResponse<T>> {
        let status = response.status;
        let metadata = metadata(status, &response.headers);
        if !(200..300).contains(&status) && !accept_non_success(status) {
            return Err(self.decode_error(response).await);
        }
        let body = collect_body(response, self.config.max_response_bytes).await?;
        let value: serde_json::Value = serde_json::from_slice(&body)
            .map_err(|error| RelaySdkError::InvalidJson(error.to_string()))?;
        let usage = Usage::from_response(&value)
            .map_err(|error| RelaySdkError::InvalidJson(error.to_string()))?;
        let data = serde_json::from_value(value)
            .map_err(|error| RelaySdkError::InvalidJson(error.to_string()))?;
        Ok(RelayResponse {
            data,
            usage,
            metadata,
        })
    }

    async fn decode_error(&self, response: HttpResponse) -> RelaySdkError {
        let status = response.status;
        let headers = response.headers.clone();
        let body = match collect_body(response, self.config.max_response_bytes).await {
            Ok(body) => body,
            Err(error) => return error,
        };
        if status == 402 {
            return match decode_payment_required(&headers) {
                Ok(required) => RelaySdkError::PaymentRequired(Box::new(required)),
                Err(error) => error,
            };
        }
        if status == 409
            && let Ok(replay) = serde_json::from_slice::<SettledReplay>(&body)
            && replay.error.code.as_deref() == Some("request_already_settled")
        {
            let Some(validator) = &self.config.evidence_validator else {
                return RelaySdkError::UnverifiedSettlementEvidence(Box::new(replay.data));
            };
            return match validator.validate(&replay.data) {
                Ok(()) => RelaySdkError::AlreadySettled(Box::new(replay.data)),
                Err(error) => RelaySdkError::InvalidSettlementEvidence(error.to_string()),
            };
        }
        let envelope = serde_json::from_slice::<ApiErrorEnvelope>(&body).ok();
        let message = envelope.as_ref().map_or_else(
            || fallback_error_message(&body),
            |value| value.message().into(),
        );
        RelaySdkError::Api(Box::new(ApiFailure {
            status,
            message,
            envelope,
            body,
        }))
    }
}

fn validate_options(options: &RequestOptions) -> Result<()> {
    if options.aitp_authorization.is_some() != options.aitp_request_id.is_some() {
        return Err(RelaySdkError::InvalidRequest(
            "AITP paid retry requires both authorization and request id".into(),
        ));
    }
    Ok(())
}

fn validate_relative_path(path: &str) -> Result<()> {
    if path.is_empty()
        || path.contains("://")
        || path.split('/').any(|segment| segment == "..")
        || path.contains(['\r', '\n'])
    {
        return Err(RelaySdkError::InvalidRequest(
            "Relay endpoint path must be a safe relative path".into(),
        ));
    }
    Ok(())
}

fn insert_header(headers: &mut HeaderMap, name: HeaderName, value: &str) -> Result<()> {
    let value = HeaderValue::from_str(value)
        .map_err(|_| RelaySdkError::InvalidRequest("invalid HTTP header value".into()))?;
    headers.insert(name, value);
    Ok(())
}

fn insert_static_header(headers: &mut HeaderMap, name: &'static str, value: &str) -> Result<()> {
    insert_header(headers, HeaderName::from_static(name), value)
}

fn metadata(status: u16, headers: &HeaderMap) -> RelayResponseMetadata {
    RelayResponseMetadata {
        status,
        request_id: header(headers, AITP_REQUEST_ID_HEADER)
            .and_then(|raw| Uuid::parse_str(raw).ok()),
        aitp_receipt: header(headers, AITP_RECEIPT_HEADER).map(str::to_owned),
        aitp_version: header(headers, AITP_VERSION_HEADER).map(str::to_owned),
        billing_overage: header(headers, RELAY_BILLING_OVERAGE_HEADER).map(str::to_owned),
        headers: headers.clone(),
    }
}

fn header<'a>(headers: &'a HeaderMap, name: &'static str) -> Option<&'a str> {
    headers.get(name)?.to_str().ok()
}

fn decode_payment_required(headers: &HeaderMap) -> Result<PaymentRequired> {
    let encoded = header(headers, AITP_CHALLENGE_HEADER).ok_or_else(|| {
        RelaySdkError::InvalidPaymentChallenge("missing aitp-challenge header".into())
    })?;
    let challenge = AitpChallenge::decode(encoded)
        .map_err(|error| RelaySdkError::InvalidPaymentChallenge(error.to_string()))?;
    let request_id = header(headers, AITP_REQUEST_ID_HEADER)
        .map(Uuid::parse_str)
        .transpose()
        .map_err(|_| RelaySdkError::InvalidPaymentChallenge("invalid request id".into()))?;
    Ok(PaymentRequired {
        challenge,
        request_id,
        protocol_version: header(headers, AITP_VERSION_HEADER).map(str::to_owned),
    })
}

fn fallback_error_message(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let text = text.trim();
    if text.is_empty() {
        "Relay returned an error without a response body".into()
    } else {
        text.chars().take(512).collect()
    }
}

async fn collect_body(response: HttpResponse, limit: usize) -> Result<Vec<u8>> {
    let mut body = response.body;
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk?;
        let next_len = bytes
            .len()
            .checked_add(chunk.len())
            .ok_or(RelaySdkError::ResponseTooLarge { limit })?;
        if next_len > limit {
            return Err(RelaySdkError::ResponseTooLarge { limit });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use bytes::Bytes;
    use futures::stream;
    use http::header::CONTENT_TYPE;
    use relay_contract::{AitpScheme, Microdollars};
    use std::sync::Mutex;

    use super::*;
    use crate::RelayCredential;

    struct FakeTransport {
        response: Mutex<Option<HttpResponse>>,
        request: Mutex<Option<HttpRequest>>,
    }

    #[async_trait]
    impl HttpTransport for FakeTransport {
        async fn send(&self, request: HttpRequest) -> Result<HttpResponse> {
            *self.request.lock().unwrap() = Some(request);
            self.response
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| RelaySdkError::Transport("no fake response".into()))
        }
    }

    fn response(status: u16, headers: HeaderMap, body: &'static [u8]) -> HttpResponse {
        HttpResponse {
            status,
            headers,
            body: Box::pin(stream::once(async move { Ok(Bytes::from_static(body)) })),
        }
    }

    fn owned_response(status: u16, body: Vec<u8>) -> HttpResponse {
        HttpResponse {
            status,
            headers: HeaderMap::new(),
            body: Box::pin(stream::iter([Ok(Bytes::from(body))])),
        }
    }

    fn client(fake: Arc<FakeTransport>) -> RelayClient {
        let config = RelayClientConfig::builder("https://relay.test/root")
            .unwrap()
            .credential(RelayCredential::bearer("secret").unwrap())
            .build()
            .unwrap();
        RelayClient::with_transport(config, fake)
    }

    #[tokio::test]
    async fn json_call_applies_dryrun_auth_and_parses_usage() {
        let fake = Arc::new(FakeTransport {
            response: Mutex::new(Some(response(
                200,
                HeaderMap::new(),
                br#"{"choices":[],"usage":{"prompt_tokens":4,"completion_tokens":2,"tensorzero_cost":0.000006}}"#,
            ))),
            request: Mutex::new(None),
        });
        let result: RelayResponse<serde_json::Value> = client(fake.clone())
            .chat_completions(
                serde_json::json!({"model":"tensorzero::model_name::openai::gpt-4o"}),
                RequestOptions {
                    dry_run: true,
                    ..RequestOptions::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            result.usage.unwrap().provider_cost_micros,
            Some(Microdollars::new(6))
        );
        let request = fake.request.lock().unwrap();
        let request = request.as_ref().unwrap();
        assert_eq!(request.headers[AUTHORIZATION], "Bearer secret");
        assert_eq!(request.headers[CONTENT_TYPE], "application/json");
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        assert_eq!(body["tensorzero::dryrun"], true);
        assert_eq!(
            request.url.as_str(),
            "https://relay.test/root/openai/v1/chat/completions"
        );
    }

    #[tokio::test]
    async fn payment_required_is_typed_from_headers() {
        let challenge = AitpChallenge {
            payee: "ait1qrelay".into(),
            amount_micros: Microdollars::new(50),
            currency: "AIT".into(),
            resource: "POST /openai/v1/chat/completions#m@id".into(),
            nonce: "nonce".into(),
            expiry: u64::MAX,
            scheme: AitpScheme::Hold,
        };
        let request_id = Uuid::now_v7();
        let mut headers = HeaderMap::new();
        insert_static_header(
            &mut headers,
            AITP_CHALLENGE_HEADER,
            &challenge.encode().unwrap(),
        )
        .unwrap();
        insert_static_header(
            &mut headers,
            AITP_REQUEST_ID_HEADER,
            &request_id.to_string(),
        )
        .unwrap();
        let fake = Arc::new(FakeTransport {
            response: Mutex::new(Some(response(402, headers, b"{}"))),
            request: Mutex::new(None),
        });
        let error = client(fake)
            .chat_completions::<serde_json::Value>(serde_json::json!({}), RequestOptions::default())
            .await
            .unwrap_err();
        let RelaySdkError::PaymentRequired(required) = error else {
            panic!("expected payment required");
        };
        assert_eq!(required.request_id, Some(request_id));
        assert_eq!(required.challenge, challenge);
    }

    #[tokio::test]
    async fn settled_replay_without_a_pinned_validator_fails_closed() {
        let fake = Arc::new(FakeTransport {
            response: Mutex::new(Some(response(
                409,
                HeaderMap::new(),
                include_bytes!("../../relay-contract/tests/fixtures/settled_replay.json"),
            ))),
            request: Mutex::new(None),
        });
        let error = client(fake)
            .inference::<serde_json::Value>(
                serde_json::json!({"model_name":"production-chat"}),
                RequestOptions::default(),
            )
            .await
            .unwrap_err();
        let RelaySdkError::UnverifiedSettlementEvidence(evidence) = error else {
            panic!("settlement evidence must not be trusted without a pinned verifier");
        };
        assert_eq!(evidence.principal, "acct_relay_owner");
    }

    #[tokio::test]
    async fn direct_settlement_lookup_is_verified_by_default() {
        let request_id = Uuid::parse_str("018f7e23-7b55-7d9a-8000-000000000001").unwrap();
        let fake = Arc::new(FakeTransport {
            response: Mutex::new(Some(response(
                200,
                HeaderMap::new(),
                include_bytes!("../../relay-contract/tests/fixtures/settlement_evidence.json"),
            ))),
            request: Mutex::new(None),
        });
        let error = client(fake)
            .settlement_evidence(request_id)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            RelaySdkError::UnverifiedSettlementEvidence(_)
        ));
    }

    #[tokio::test]
    async fn health_preserves_typed_dependency_snapshot_on_service_unavailable() {
        let fake = Arc::new(FakeTransport {
            response: Mutex::new(Some(response(
                503,
                HeaderMap::new(),
                br#"{"gateway":"ok","clickhouse":"ok","postgres":"error","valkey":"ok","valkey_cache":"ok"}"#,
            ))),
            request: Mutex::new(None),
        });

        let result = client(fake.clone()).health().await.unwrap();
        assert_eq!(result.metadata.status, 503);
        assert!(!result.data.is_healthy());
        assert_eq!(result.data.postgres, "error");
        let request = fake.request.lock().unwrap();
        let request = request.as_ref().unwrap();
        assert_eq!(request.method, http::Method::GET);
        assert_eq!(request.url.path(), "/root/health");
        assert!(request.body.is_empty());
    }

    #[tokio::test]
    async fn stream_decodes_split_terminal_usage() {
        let chunks = vec![
            Ok(Bytes::from_static(b"data: {\"usage\":null}\n\nda")),
            Ok(Bytes::from_static(
                b"ta: {\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}\n\ndata: [DONE]\n\n",
            )),
        ];
        let fake = Arc::new(FakeTransport {
            response: Mutex::new(Some(HttpResponse {
                status: 200,
                headers: HeaderMap::new(),
                body: Box::pin(stream::iter(chunks)),
            })),
            request: Mutex::new(None),
        });
        let mut relay_stream = client(fake)
            .chat_completions_stream(
                serde_json::json!({"stream":true}),
                RequestOptions::default(),
            )
            .await
            .unwrap();
        while relay_stream.next_event().await.unwrap().is_some() {}
        assert_eq!(relay_stream.terminal_usage().unwrap().total_tokens, 3);
    }

    #[tokio::test]
    async fn catalog_and_quote_use_typed_data_envelopes() {
        let snapshot = relay_catalog::CatalogSnapshot::new("revision-1", 123, Vec::new());
        let fake = Arc::new(FakeTransport {
            response: Mutex::new(Some(owned_response(
                200,
                serde_json::to_vec(&DataEnvelope {
                    data: snapshot.clone(),
                })
                .unwrap(),
            ))),
            request: Mutex::new(None),
        });
        assert_eq!(client(fake.clone()).catalog().await.unwrap().data, snapshot);
        assert_eq!(
            fake.request.lock().unwrap().as_ref().unwrap().url.path(),
            "/root/v1/relay/catalog"
        );

        let quote = relay_catalog::PriceQuote {
            model: "openai/gpt-4o".into(),
            input_tokens: 1_000,
            output_tokens: 500,
            amount_micros: Microdollars::new(7_500),
            pricing: relay_catalog::ModelPrice::new(
                2_500_000,
                10_000_000,
                relay_catalog::PricingSource::ProviderList,
            ),
            catalog_revision: "revision-1".into(),
        };
        let fake = Arc::new(FakeTransport {
            response: Mutex::new(Some(owned_response(
                200,
                serde_json::to_vec(&DataEnvelope {
                    data: quote.clone(),
                })
                .unwrap(),
            ))),
            request: Mutex::new(None),
        });
        let returned = client(fake.clone())
            .quote(relay_catalog::PriceQuoteRequest {
                model: "openai/gpt-4o".into(),
                input_tokens: 1_000,
                output_tokens: 500,
            })
            .await
            .unwrap();
        assert_eq!(returned.data, quote);
        let request = fake.request.lock().unwrap();
        let request = request.as_ref().unwrap();
        assert_eq!(request.method, http::Method::POST);
        assert_eq!(request.url.path(), "/root/v1/relay/quotes");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&request.body).unwrap(),
            serde_json::json!({
                "model": "openai/gpt-4o",
                "input_tokens": 1_000,
                "output_tokens": 500
            })
        );
    }

    #[tokio::test]
    async fn cost_event_listing_is_typed_and_rejects_an_unbounded_query_locally() {
        let page = CostEventPage {
            items: Vec::new(),
            next_cursor: None,
        };
        let fake = Arc::new(FakeTransport {
            response: Mutex::new(Some(owned_response(
                200,
                serde_json::to_vec(&DataEnvelope { data: page.clone() }).unwrap(),
            ))),
            request: Mutex::new(None),
        });
        let result = client(fake.clone())
            .cost_events(CostEventListQuery {
                state: Some(relay_cost_feed::CostEventState::Pending),
                before: None,
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(result.data, page);
        assert_eq!(
            fake.request.lock().unwrap().as_ref().unwrap().url.path(),
            "/root/v1/billing/cost-events"
        );
        assert_eq!(
            fake.request.lock().unwrap().as_ref().unwrap().url.query(),
            Some("limit=10&state=pending")
        );

        let error = client(Arc::new(FakeTransport {
            response: Mutex::new(None),
            request: Mutex::new(None),
        }))
        .cost_events(CostEventListQuery {
            limit: relay_cost_feed::MAX_LIST_LIMIT + 1,
            ..CostEventListQuery::default()
        })
        .await
        .unwrap_err();
        assert!(matches!(error, RelaySdkError::InvalidRequest(_)));
    }
}
