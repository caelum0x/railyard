//! [`RelayModel`] — a rig [`CompletionModel`] backed by Relay.
//!
//! It genuinely reuses the vendored `rig_core` openai **Chat Completions**
//! client (NOT the default Responses API client, which POSTs `/responses` and
//! Relay does not route). By default it wraps that client; because it is
//! generic over the inner model it can also wrap a `MockCompletionModel` in
//! tests, or later be composed under the AITP-402 paid decorator.

use rig_core::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, ProviderCapabilities,
};
use rig_core::providers::openai;
use rig_core::streaming::StreamingCompletionResponse;
use rig_core::wasm_compat::WasmCompatSend;

use crate::config::RelayConfig;
use crate::error::RelayError;

/// The concrete inner model when connecting to a live Relay: rig's openai
/// Chat Completions model over the default `reqwest` backend.
// `openai::CompletionModel`'s HTTP-backend type param defaults to
// `reqwest::Client`, so we never have to name `reqwest` directly.
pub type OpenAiCompletions = openai::CompletionModel;

/// A completion model that speaks to Relay's OpenAI-compatible gateway.
///
/// Generic over the inner rig model `M` so the same wrapper works for the real
/// openai client and for `MockCompletionModel` in tests.
#[derive(Clone)]
pub struct RelayModel<M = OpenAiCompletions> {
    inner: M,
}

impl<M> RelayModel<M> {
    /// Wrap an already-constructed rig model (e.g. a mock in tests).
    pub fn from_inner(inner: M) -> Self {
        Self { inner }
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
        Ok(Self { inner })
    }
}

impl<M> CompletionModel for RelayModel<M>
where
    M: CompletionModel,
{
    fn completion(
        &self,
        request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<CompletionResponse, CompletionError>> + WasmCompatSend
    {
        self.inner.completion(request)
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<StreamingCompletionResponse, CompletionError>>
           + WasmCompatSend {
        self.inner.stream(request)
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }
}
