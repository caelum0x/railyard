use async_trait::async_trait;
use relay_contract::PaymentRequired;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{
    RelayClient, RelayEndpoint, RelayResponse, RelaySdkError, RelayStream, RequestOptions, Result,
};

/// Injected payer seam. Implementations may use Mint, a hardware signer, an
/// agent wallet, or an approval policy; the SDK never owns private key material.
#[async_trait]
pub trait AitpAuthorizer: Send + Sync {
    async fn authorize(&self, payment: &PaymentRequired) -> Result<String>;
}

impl RelayClient {
    /// Execute one JSON request, recover one AITP-402 challenge, and retry
    /// exactly once. A second 402 is returned to the caller; it is never looped.
    pub async fn paid_json<T: DeserializeOwned>(
        &self,
        endpoint: RelayEndpoint,
        body: impl Serialize,
        mut options: RequestOptions,
        authorizer: &dyn AitpAuthorizer,
    ) -> Result<RelayResponse<T>> {
        reject_pre_authorized_options(&options)?;
        let body = serde_json::to_value(body)
            .map_err(|error| RelaySdkError::InvalidRequest(error.to_string()))?;
        match self
            .json::<T>(endpoint.clone(), body.clone(), options.clone())
            .await
        {
            Ok(response) => Ok(response),
            Err(RelaySdkError::PaymentRequired(payment)) => {
                apply_authorization(&mut options, &payment, authorizer).await?;
                self.json(endpoint, body, options).await
            }
            Err(error) => Err(error),
        }
    }

    pub async fn paid_chat_completions<T: DeserializeOwned>(
        &self,
        body: impl Serialize,
        options: RequestOptions,
        authorizer: &dyn AitpAuthorizer,
    ) -> Result<RelayResponse<T>> {
        self.paid_json(
            RelayEndpoint::OpenAiChatCompletions,
            body,
            options,
            authorizer,
        )
        .await
    }

    /// Execute one streaming request, recover one AITP-402 challenge, and
    /// return the paid stream. The response body is not opened until Relay has
    /// accepted the authorization, and a second challenge is never looped.
    pub async fn paid_json_stream(
        &self,
        endpoint: RelayEndpoint,
        body: impl Serialize,
        mut options: RequestOptions,
        authorizer: &dyn AitpAuthorizer,
    ) -> Result<RelayStream> {
        reject_pre_authorized_options(&options)?;
        let body = serde_json::to_value(body)
            .map_err(|error| RelaySdkError::InvalidRequest(error.to_string()))?;
        match self
            .json_stream(endpoint.clone(), body.clone(), options.clone())
            .await
        {
            Ok(response) => Ok(response),
            Err(RelaySdkError::PaymentRequired(payment)) => {
                apply_authorization(&mut options, &payment, authorizer).await?;
                self.json_stream(endpoint, body, options).await
            }
            Err(error) => Err(error),
        }
    }

    pub async fn paid_chat_completions_stream(
        &self,
        body: impl Serialize,
        options: RequestOptions,
        authorizer: &dyn AitpAuthorizer,
    ) -> Result<RelayStream> {
        self.paid_json_stream(
            RelayEndpoint::OpenAiChatCompletions,
            body,
            options,
            authorizer,
        )
        .await
    }

    pub async fn paid_embeddings<T: DeserializeOwned>(
        &self,
        body: impl Serialize,
        options: RequestOptions,
        authorizer: &dyn AitpAuthorizer,
    ) -> Result<RelayResponse<T>> {
        self.paid_json(RelayEndpoint::OpenAiEmbeddings, body, options, authorizer)
            .await
    }

    pub async fn paid_inference<T: DeserializeOwned>(
        &self,
        body: impl Serialize,
        options: RequestOptions,
        authorizer: &dyn AitpAuthorizer,
    ) -> Result<RelayResponse<T>> {
        self.paid_json(RelayEndpoint::NativeInference, body, options, authorizer)
            .await
    }

    pub async fn paid_inference_stream(
        &self,
        body: impl Serialize,
        options: RequestOptions,
        authorizer: &dyn AitpAuthorizer,
    ) -> Result<RelayStream> {
        self.paid_json_stream(RelayEndpoint::NativeInference, body, options, authorizer)
            .await
    }

    pub async fn paid_batch_inference<T: DeserializeOwned>(
        &self,
        body: impl Serialize,
        options: RequestOptions,
        authorizer: &dyn AitpAuthorizer,
    ) -> Result<RelayResponse<T>> {
        self.paid_json(
            RelayEndpoint::NativeBatchInference,
            body,
            options,
            authorizer,
        )
        .await
    }
}

fn reject_pre_authorized_options(options: &RequestOptions) -> Result<()> {
    if options.aitp_authorization.is_some() || options.aitp_request_id.is_some() {
        return Err(RelaySdkError::InvalidRequest(
            "paid request owns AITP retry headers; initial options must not contain them".into(),
        ));
    }
    Ok(())
}

async fn apply_authorization(
    options: &mut RequestOptions,
    payment: &PaymentRequired,
    authorizer: &dyn AitpAuthorizer,
) -> Result<()> {
    let request_id = payment.request_id.ok_or_else(|| {
        RelaySdkError::InvalidPaymentChallenge("payment challenge is missing its request id".into())
    })?;
    let authorization = authorizer.authorize(payment).await?;
    if authorization.trim().is_empty() || authorization.contains(['\r', '\n']) {
        return Err(RelaySdkError::InvalidRequest(
            "AITP authorizer returned an invalid header value".into(),
        ));
    }
    options.aitp_authorization = Some(authorization);
    options.aitp_request_id = Some(request_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use bytes::Bytes;
    use futures::stream;
    use http::HeaderMap;
    use relay_contract::headers::{AITP_CHALLENGE_HEADER, AITP_REQUEST_ID_HEADER};
    use relay_contract::{AitpChallenge, AitpScheme, Microdollars};
    use uuid::Uuid;

    use super::*;
    use crate::{HttpRequest, HttpResponse, HttpTransport, RelayClientConfig, RelayCredential};

    struct QueueTransport {
        responses: Mutex<VecDeque<HttpResponse>>,
        requests: Mutex<Vec<HttpRequest>>,
    }

    #[async_trait]
    impl HttpTransport for QueueTransport {
        async fn send(&self, request: HttpRequest) -> Result<HttpResponse> {
            self.requests.lock().unwrap().push(request);
            self.responses
                .lock()
                .unwrap()
                .pop_front()
                .ok_or_else(|| RelaySdkError::Transport("no queued response".into()))
        }
    }

    struct StaticAuthorizer;

    struct TestEvidenceValidator;

    impl crate::SettlementEvidenceValidator for TestEvidenceValidator {
        fn validate(
            &self,
            _evidence: &relay_contract::SettlementEvidence,
        ) -> std::result::Result<(), crate::EvidenceValidationError> {
            Ok(())
        }
    }

    #[async_trait]
    impl AitpAuthorizer for StaticAuthorizer {
        async fn authorize(&self, _payment: &PaymentRequired) -> Result<String> {
            Ok("signed-authorization".into())
        }
    }

    fn response(status: u16, headers: HeaderMap, body: &'static [u8]) -> HttpResponse {
        HttpResponse {
            status,
            headers,
            body: Box::pin(stream::once(async move { Ok(Bytes::from_static(body)) })),
        }
    }

    #[tokio::test]
    async fn paid_journey_retries_once_with_server_request_id() {
        let request_id = Uuid::now_v7();
        let challenge = AitpChallenge {
            payee: "ait1qrelay".into(),
            amount_micros: Microdollars::new(50),
            currency: "AIT".into(),
            resource: format!("POST /openai/v1/chat/completions#m@{request_id}"),
            nonce: "nonce".into(),
            expiry: u64::MAX,
            scheme: AitpScheme::Hold,
        };
        let mut payment_headers = HeaderMap::new();
        payment_headers.insert(
            AITP_CHALLENGE_HEADER,
            challenge.encode().unwrap().parse().unwrap(),
        );
        payment_headers.insert(
            AITP_REQUEST_ID_HEADER,
            request_id.to_string().parse().unwrap(),
        );
        let transport = Arc::new(QueueTransport {
            responses: Mutex::new(VecDeque::from([
                response(402, payment_headers, b"{}"),
                response(200, HeaderMap::new(), br#"{"choices":[]}"#),
            ])),
            requests: Mutex::new(Vec::new()),
        });
        let config = RelayClientConfig::builder("https://relay.test")
            .unwrap()
            .credential(RelayCredential::bearer("relay-key").unwrap())
            .build()
            .unwrap();
        let client = RelayClient::with_transport(config, transport.clone());
        let result: RelayResponse<serde_json::Value> = client
            .paid_chat_completions(
                serde_json::json!({"model":"m"}),
                RequestOptions::default(),
                &StaticAuthorizer,
            )
            .await
            .unwrap();
        assert_eq!(result.metadata.status, 200);
        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(requests[0].headers.get("aitp-authorization").is_none());
        assert_eq!(
            requests[1].headers["aitp-authorization"],
            "signed-authorization"
        );
        assert_eq!(
            requests[1].headers["aitp-request-id"],
            request_id.to_string()
        );
    }

    #[tokio::test]
    async fn paid_stream_retries_before_exposing_the_response_body() {
        let request_id = Uuid::now_v7();
        let challenge = AitpChallenge {
            payee: "ait1qrelay".into(),
            amount_micros: Microdollars::new(75),
            currency: "AIT".into(),
            resource: format!("POST /openai/v1/chat/completions#m@{request_id}"),
            nonce: "stream-nonce".into(),
            expiry: u64::MAX,
            scheme: AitpScheme::Hold,
        };
        let mut payment_headers = HeaderMap::new();
        payment_headers.insert(
            AITP_CHALLENGE_HEADER,
            challenge.encode().unwrap().parse().unwrap(),
        );
        payment_headers.insert(
            AITP_REQUEST_ID_HEADER,
            request_id.to_string().parse().unwrap(),
        );
        let mut stream_headers = HeaderMap::new();
        stream_headers.insert(
            AITP_REQUEST_ID_HEADER,
            request_id.to_string().parse().unwrap(),
        );
        let transport = Arc::new(QueueTransport {
            responses: Mutex::new(VecDeque::from([
                response(402, payment_headers, b"{}"),
                response(
                    200,
                    stream_headers,
                    b"data: {\"delta\":\"hello\"}\n\ndata: [DONE]\n\n",
                ),
            ])),
            requests: Mutex::new(Vec::new()),
        });
        let config = RelayClientConfig::builder("https://relay.test")
            .unwrap()
            .build()
            .unwrap();
        let client = RelayClient::with_transport(config, transport.clone());

        let mut stream = client
            .paid_chat_completions_stream(
                serde_json::json!({"model":"m", "stream":true}),
                RequestOptions::default(),
                &StaticAuthorizer,
            )
            .await
            .unwrap();
        assert_eq!(stream.metadata().request_id, Some(request_id));
        assert_eq!(
            stream.next_event().await.unwrap().unwrap().data.as_deref(),
            Some("{\"delta\":\"hello\"}")
        );
        assert_eq!(
            stream.next_event().await.unwrap().unwrap().data.as_deref(),
            Some("[DONE]")
        );
        assert!(stream.next_event().await.unwrap().is_none());

        let requests = transport.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            requests[0].headers[http::header::ACCEPT],
            "text/event-stream"
        );
        assert!(requests[0].headers.get("aitp-authorization").is_none());
        assert_eq!(
            requests[1].headers["aitp-authorization"],
            "signed-authorization"
        );
    }

    #[tokio::test]
    async fn a_second_payment_challenge_is_returned_without_a_retry_loop() {
        let request_id = Uuid::now_v7();
        let challenge = AitpChallenge {
            payee: "ait1qrelay".into(),
            amount_micros: Microdollars::new(25),
            currency: "AIT".into(),
            resource: format!("POST /inference#m@{request_id}"),
            nonce: "repeat-nonce".into(),
            expiry: u64::MAX,
            scheme: AitpScheme::Hold,
        };
        let mut payment_headers = HeaderMap::new();
        payment_headers.insert(
            AITP_CHALLENGE_HEADER,
            challenge.encode().unwrap().parse().unwrap(),
        );
        payment_headers.insert(
            AITP_REQUEST_ID_HEADER,
            request_id.to_string().parse().unwrap(),
        );
        let transport = Arc::new(QueueTransport {
            responses: Mutex::new(VecDeque::from([
                response(402, payment_headers.clone(), b"{}"),
                response(402, payment_headers, b"{}"),
            ])),
            requests: Mutex::new(Vec::new()),
        });
        let config = RelayClientConfig::builder("https://relay.test")
            .unwrap()
            .build()
            .unwrap();
        let client = RelayClient::with_transport(config, transport.clone());

        let error = client
            .paid_inference::<serde_json::Value>(
                serde_json::json!({"model_name":"m"}),
                RequestOptions::default(),
                &StaticAuthorizer,
            )
            .await
            .unwrap_err();
        assert!(matches!(error, RelaySdkError::PaymentRequired(_)));
        assert_eq!(transport.requests.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn settled_retry_returns_typed_terminal_evidence() {
        let request_id = Uuid::parse_str("018f3c20-c57a-7a4b-8f7e-1ed8e42b63c0").unwrap();
        let challenge = AitpChallenge {
            payee: "ait1qrelay".into(),
            amount_micros: Microdollars::new(25),
            currency: "AIT".into(),
            resource: format!("POST /inference#m@{request_id}"),
            nonce: "settled-nonce".into(),
            expiry: u64::MAX,
            scheme: AitpScheme::Hold,
        };
        let mut payment_headers = HeaderMap::new();
        payment_headers.insert(
            AITP_CHALLENGE_HEADER,
            challenge.encode().unwrap().parse().unwrap(),
        );
        payment_headers.insert(
            AITP_REQUEST_ID_HEADER,
            request_id.to_string().parse().unwrap(),
        );
        let transport = Arc::new(QueueTransport {
            responses: Mutex::new(VecDeque::from([
                response(402, payment_headers, b"{}"),
                response(
                    409,
                    HeaderMap::new(),
                    br#"{
                        "error": {
                            "message": "request already settled",
                            "type": "request_already_settled",
                            "code": "request_already_settled"
                        },
                        "data": {
                            "request_id": "018f3c20-c57a-7a4b-8f7e-1ed8e42b63c0",
                            "principal": "acct_owner",
                            "receipt": {
                                "sequence": 7,
                                "operation": "hold.capture",
                                "operation_hash": "op",
                                "amount_micros": 42,
                                "receipt_hash": "hash",
                                "signature": "sig"
                            },
                            "settled_at_ms": 123
                        }
                    }"#,
                ),
            ])),
            requests: Mutex::new(Vec::new()),
        });
        let config = RelayClientConfig::builder("https://relay.test")
            .unwrap()
            .settlement_evidence_validator(Arc::new(TestEvidenceValidator))
            .build()
            .unwrap();
        let client = RelayClient::with_transport(config, transport.clone());

        let error = client
            .paid_inference::<serde_json::Value>(
                serde_json::json!({"model_name":"m"}),
                RequestOptions::default(),
                &StaticAuthorizer,
            )
            .await
            .unwrap_err();
        let RelaySdkError::AlreadySettled(evidence) = error else {
            panic!("expected typed settled replay");
        };
        assert_eq!(evidence.request_id, request_id);
        assert_eq!(evidence.receipt.amount_micros.get(), 42);
        assert_eq!(transport.requests.lock().unwrap().len(), 2);
    }
}
