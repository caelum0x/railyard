//! [`RelayModel`] — a rig [`CompletionModel`] backed by Relay.
//!
//! It genuinely reuses the vendored `rig_core` openai **Chat Completions**
//! client (NOT the default Responses API client, which POSTs `/responses` and
//! Relay does not route). By default it wraps that client; because it is
//! generic over the inner model it can also wrap a `MockCompletionModel` in
//! tests, or later be composed under the AITP-402 paid decorator.

use rig_core::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse,
};
use rig_core::providers::openai;
use rig_core::streaming::StreamingCompletionResponse;
use rig_core::wasm_compat::WasmCompatSend;
use std::sync::Arc;

use aitp_client::{AmountMicros, AuthSigner, HttpTransport, SettlementEvidenceVerifier};

use crate::config::RelayConfig;
use crate::error::RelayError;
use crate::paid_transport::AitpRigHttpClient;

/// The concrete inner model when connecting to a live Relay: rig's openai
/// Chat Completions model over the default `reqwest` backend.
// `openai::CompletionModel`'s HTTP-backend type param defaults to
// `reqwest::Client`, so we never have to name `reqwest` directly.
pub type OpenAiCompletions = openai::CompletionModel;

/// Rig's OpenAI Chat Completions model over the AITP-aware HTTP backend.
pub type PaidOpenAiCompletions = openai::CompletionModel<AitpRigHttpClient>;

/// A completion model that speaks to Relay's OpenAI-compatible gateway.
///
/// Generic over the inner rig model `M` so the same wrapper works for the real
/// openai client and for `MockCompletionModel` in tests.
#[derive(Clone)]
pub struct RelayModel<M = OpenAiCompletions> {
    inner: M,
    payment_http: Option<AitpRigHttpClient>,
}

impl<M> RelayModel<M> {
    /// Wrap an already-constructed rig model (e.g. a mock in tests).
    pub fn from_inner(inner: M) -> Self {
        Self {
            inner,
            payment_http: None,
        }
    }

    /// Borrow the inner model.
    pub fn inner(&self) -> &M {
        &self.inner
    }
}

impl RelayModel<OpenAiCompletions> {
    /// Build a live Relay-backed model from config.
    ///
    /// Uses `openai::Client::builder().completions_api()` so requests hit
    /// `{base_url}/chat/completions`, with Bearer auth wired from the api key.
    pub fn connect(config: &RelayConfig) -> Result<Self, RelayError> {
        use rig_core::client::completion::CompletionClient;

        let client = openai::Client::builder()
            .api_key(config.api_key.clone())
            .base_url(&config.base_url)
            .build()
            .map_err(|e| RelayError::Build(e.to_string()))?
            .completions_api();

        let inner = client.completion_model(config.model.clone());
        Ok(Self {
            inner,
            payment_http: None,
        })
    }
}

impl RelayModel<PaidOpenAiCompletions> {
    /// Build a high-level Relay model that answers one policy-approved AITP 402.
    ///
    /// Relay owns pricing, reservation, usage settlement and evidence. This
    /// caller only signs the server challenge and retries the identical Rig
    /// request once; it must not be wrapped in `PaidCompletionModel`.
    pub fn connect_paid(
        config: &RelayConfig,
        payer: impl Into<String>,
        expected_payee: impl Into<String>,
        max_amount_micros: AmountMicros,
        signer: Arc<dyn AuthSigner>,
        evidence_verifier: SettlementEvidenceVerifier,
    ) -> Result<Self, RelayError> {
        let policy = config.payment_policy(expected_payee, max_amount_micros)?;
        let http = AitpRigHttpClient::new(
            config.base_url.clone(),
            payer,
            signer,
            policy,
            evidence_verifier,
        )?;
        Self::connect_with_http(config, http)
    }

    /// Deterministic transport injection for product tests and embedded hosts.
    pub fn connect_paid_with_transport(
        config: &RelayConfig,
        payer: impl Into<String>,
        expected_payee: impl Into<String>,
        max_amount_micros: AmountMicros,
        signer: Arc<dyn AuthSigner>,
        evidence_verifier: SettlementEvidenceVerifier,
        transport: Arc<dyn HttpTransport>,
    ) -> Result<Self, RelayError> {
        let policy = config.payment_policy(expected_payee, max_amount_micros)?;
        let http = AitpRigHttpClient::with_transport(
            config.base_url.clone(),
            payer,
            signer,
            policy,
            evidence_verifier,
            transport,
        );
        Self::connect_with_http(config, http)
    }

    fn connect_with_http(
        config: &RelayConfig,
        http: AitpRigHttpClient,
    ) -> Result<Self, RelayError> {
        use rig_core::client::completion::CompletionClient;

        let client = openai::Client::builder()
            .api_key(config.api_key.clone())
            .base_url(&config.base_url)
            .http_client(http.clone())
            .build()
            .map_err(|error| RelayError::Build(error.to_string()))?
            .completions_api();
        Ok(Self {
            inner: client.completion_model(config.model.clone()),
            payment_http: Some(http),
        })
    }

    /// Drain signed settlement receipts completed by paid streaming requests
    /// made through this model.
    pub fn drain_payment_evidence(&self) -> Vec<aitp_client::PaymentEvidence> {
        self.payment_http
            .as_ref()
            .map(AitpRigHttpClient::drain_payment_evidence)
            .unwrap_or_default()
    }

    /// Paid requests that still need a locally verified Mint proof.
    pub fn pending_payment_request_ids(&self) -> Vec<String> {
        self.payment_http
            .as_ref()
            .map(AitpRigHttpClient::pending_payment_request_ids)
            .unwrap_or_default()
    }

    /// Retry all unclaimed settlement lookups once, for graceful shutdown or
    /// hosts where the drop-time background reconciler could not be spawned.
    pub async fn reconcile_payment_evidence(
        &self,
    ) -> Result<Vec<aitp_client::PaymentEvidence>, aitp_client::PayError> {
        match self.payment_http.as_ref() {
            Some(http) => http.reconcile_payment_evidence().await,
            None => Ok(Vec::new()),
        }
    }
}

impl<M> CompletionModel for RelayModel<M>
where
    M: CompletionModel,
{
    type Response = M::Response;
    type StreamingResponse = M::StreamingResponse;
    type Client = M::Client;

    fn make(client: &Self::Client, model: impl Into<String>) -> Self {
        Self::from_inner(M::make(client, model))
    }

    fn completion(
        &self,
        request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<CompletionResponse<Self::Response>, CompletionError>>
           + WasmCompatSend {
        self.inner.completion(request)
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> impl std::future::Future<
        Output = Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError>,
    > + WasmCompatSend {
        self.inner.stream(request)
    }
}
