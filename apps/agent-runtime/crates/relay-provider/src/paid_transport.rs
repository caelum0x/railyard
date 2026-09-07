//! Rig HTTP backend that reuses [`aitp_client::PaidHttpClient`].
//!
//! Relay remains the only component that reserves and settles Mint funds. This
//! adapter only answers one server-issued AITP 402 challenge, then gives Rig the
//! final response. It never invokes the direct fixed-price [`PaymentGateway`]
//! path and never invents a local price.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aitp_client::{
    AuthSigner, AuthorizationBuilder, AuthorizationPolicy, HttpRequest, HttpResponse,
    HttpTransport, PaidHttpClient, PaymentEvidence, ReqwestTransport, SettlementEvidenceVerifier,
    StreamingHttpResponse,
};
use bytes::Bytes;
use futures_util::StreamExt;
use rig_core::http_client::{self, HttpClientExt, LazyBody, MultipartForm, Request, Response};

use crate::error::RelayError;

/// Custom Rig backend for Relay's one-shot AITP 402 authorization retry.
///
/// `Default` is intentionally unconfigured because Rig's generic OpenAI model
/// requires a defaultable backend type. Any attempt to use that sentinel fails
/// closed; production constructors always install a configured instance.
#[derive(Clone, Default)]
pub struct AitpRigHttpClient {
    inner: Option<Arc<PaidHttpClient>>,
    base_url: Option<Arc<str>>,
    evidence: EvidenceState,
}

#[derive(Clone, Default)]
struct EvidenceState(Arc<Mutex<EvidenceStateInner>>);

#[derive(Default)]
struct EvidenceStateInner {
    completed: VecDeque<PaymentEvidence>,
    pending: HashMap<String, PendingEvidence>,
}

struct PendingEvidence {
    lookup_headers: Vec<(String, String)>,
    reconciling: bool,
}

impl EvidenceState {
    fn register(&self, request_id: String, lookup_headers: Vec<(String, String)>) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        state.pending.entry(request_id).or_insert(PendingEvidence {
            lookup_headers,
            reconciling: false,
        });
    }

    fn begin(&self, request_id: &str) -> Option<Vec<(String, String)>> {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let pending = state.pending.get_mut(request_id)?;
        if pending.reconciling {
            return None;
        }
        pending.reconciling = true;
        Some(pending.lookup_headers.clone())
    }

    fn begin_all(&self) -> Vec<(String, Vec<(String, String)>)> {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        state
            .pending
            .iter_mut()
            .filter_map(|(request_id, pending)| {
                if pending.reconciling {
                    None
                } else {
                    pending.reconciling = true;
                    Some((request_id.clone(), pending.lookup_headers.clone()))
                }
            })
            .collect()
    }

    fn retryable(&self, request_id: &str) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(pending) = state.pending.get_mut(request_id) {
            pending.reconciling = false;
        }
    }

    fn complete(&self, request_id: &str, evidence: PaymentEvidence) {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        state.pending.remove(request_id);
        const MAX_RETAINED_EVIDENCE: usize = 256;
        if state.completed.len() == MAX_RETAINED_EVIDENCE {
            // The buffer is full because a consumer never drained it. Dropping
            // the oldest proof is a silent loss of verified audit evidence, so
            // log it loudly rather than discarding quietly.
            let dropped = state.completed.pop_front();
            tracing::warn!(
                capacity = MAX_RETAINED_EVIDENCE,
                dropped_request_id = dropped
                    .as_ref()
                    .and_then(|evidence| evidence.request_id.as_deref())
                    .unwrap_or("<unknown>"),
                "AITP settlement-evidence buffer is full; dropping the oldest verified \
                 proof — call drain_payment_evidence() to avoid losing audit evidence"
            );
        }
        state.completed.push_back(evidence);
    }

    fn drain(&self) -> Vec<PaymentEvidence> {
        let mut state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        state.completed.drain(..).collect()
    }

    fn pending_ids(&self) -> Vec<String> {
        let state = self.0.lock().unwrap_or_else(|error| error.into_inner());
        let mut ids = state.pending.keys().cloned().collect::<Vec<_>>();
        ids.sort();
        ids
    }
}

impl fmt::Debug for AitpRigHttpClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AitpRigHttpClient")
            .field("configured", &self.inner.is_some())
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl AitpRigHttpClient {
    /// Build the production backend over a real rustls-enabled HTTP transport.
    pub fn new(
        base_url: impl Into<String>,
        payer: impl Into<String>,
        signer: Arc<dyn AuthSigner>,
        policy: AuthorizationPolicy,
        evidence_verifier: SettlementEvidenceVerifier,
    ) -> Result<Self, RelayError> {
        let base_url = normalize_base_url(base_url.into())?;
        let transport = Arc::new(
            ReqwestTransport::new(base_url.clone())
                .map_err(|error| RelayError::Build(error.to_string()))?,
        );
        Ok(Self::with_transport(
            base_url,
            payer,
            signer,
            policy,
            evidence_verifier,
            transport,
        ))
    }

    /// Inject a transport for deterministic product tests without a socket.
    pub fn with_transport(
        base_url: impl Into<String>,
        payer: impl Into<String>,
        signer: Arc<dyn AuthSigner>,
        policy: AuthorizationPolicy,
        evidence_verifier: SettlementEvidenceVerifier,
        transport: Arc<dyn HttpTransport>,
    ) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        Self {
            inner: Some(Arc::new(PaidHttpClient::new(
                transport,
                AuthorizationBuilder::new(payer, signer, policy),
                evidence_verifier,
            ))),
            base_url: Some(Arc::from(base_url)),
            evidence: EvidenceState::default(),
        }
    }

    /// Drain completed, principal-scoped settlement proofs observed by this
    /// backend. The queue is shared by all Rig client clones.
    pub fn drain_payment_evidence(&self) -> Vec<PaymentEvidence> {
        self.evidence.drain()
    }

    /// Request ids whose streams ended or were dropped before a verified Mint
    /// proof became locally available. They remain visible even when no Tokio
    /// runtime was present at drop time.
    pub fn pending_payment_request_ids(&self) -> Vec<String> {
        self.evidence.pending_ids()
    }

    /// Explicitly retry every currently unclaimed evidence lookup once. This
    /// is the deterministic recovery hook for shutdown and non-Tokio hosts;
    /// early drops also invoke the same logic in a bounded background task.
    pub async fn reconcile_payment_evidence(
        &self,
    ) -> Result<Vec<PaymentEvidence>, aitp_client::PayError> {
        let Some(inner) = self.inner.clone() else {
            return Err(aitp_client::PayError::Gateway(
                "AITP Relay HTTP backend is not configured".into(),
            ));
        };
        let mut completed = Vec::new();
        let mut first_error = None;
        for (request_id, headers) in self.evidence.begin_all() {
            match inner.settlement_evidence(&request_id, headers).await {
                Ok(evidence) if evidence.receipt.is_some() => {
                    self.evidence.complete(&request_id, evidence.clone());
                    completed.push(evidence);
                }
                Ok(_) => self.evidence.retryable(&request_id),
                Err(error) => {
                    self.evidence.retryable(&request_id);
                    if first_error.is_none() {
                        first_error = Some(error);
                    }
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(completed),
        }
    }

    fn configured(&self) -> http_client::Result<(&Arc<PaidHttpClient>, &str)> {
        match (&self.inner, &self.base_url) {
            (Some(inner), Some(base_url)) => Ok((inner, base_url)),
            _ => Err(instance_error("AITP Relay HTTP backend is not configured")),
        }
    }

    fn convert_request<T>(&self, request: Request<T>) -> http_client::Result<HttpRequest>
    where
        T: Into<Bytes>,
    {
        let (_, base_url) = self.configured()?;
        let (parts, body) = request.into_parts();
        let absolute = parts.uri.to_string();
        let path = if let Some(path) = absolute.strip_prefix(base_url) {
            path.to_string()
        } else if parts.uri.scheme().is_none() && parts.uri.authority().is_none() {
            absolute
        } else {
            return Err(instance_error(
                "Rig request escaped the configured Relay base URL",
            ));
        };
        if !path.starts_with('/') {
            return Err(instance_error("Relay request path must be absolute"));
        }
        let method = match parts.method.as_str() {
            "GET" => "GET",
            "POST" => "POST",
            _ => return Err(instance_error("unsupported Relay HTTP method")),
        };
        let mut converted = if method == "GET" {
            HttpRequest::get(path)
        } else {
            HttpRequest::post_json(path, body.into().to_vec())
        };
        converted.headers.clear();
        for (name, value) in &parts.headers {
            converted.headers.push((
                name.as_str().to_ascii_lowercase(),
                value
                    .to_str()
                    .map_err(|_| instance_error("Relay request contains a non-text header"))?
                    .to_string(),
            ));
        }
        Ok(converted)
    }

    fn convert_multipart(
        &self,
        request: Request<MultipartForm>,
    ) -> http_client::Result<HttpRequest> {
        let (parts, form) = request.into_parts();
        let (boundary, body) = form.encode();
        let request = Request::from_parts(parts, body);
        Ok(self.convert_request(request)?.with_replaced_header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        ))
    }
}

impl HttpClientExt for AitpRigHttpClient {
    fn send<T, U>(
        &self,
        request: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + Send + 'static
    where
        T: Into<Bytes> + Send,
        U: From<Bytes> + Send + 'static,
    {
        let prepared = self.convert_request(request).and_then(|request| {
            let (inner, _) = self.configured()?;
            Ok((inner.clone(), request))
        });
        async move {
            let (inner, request) = prepared?;
            let response = inner
                .send(request)
                .await
                .map_err(|error| http_client::Error::Instance(Box::new(error)))?;
            buffered_response(response)
        }
    }

    fn send_multipart<U>(
        &self,
        request: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + Send + 'static
    where
        U: From<Bytes> + Send + 'static,
    {
        let prepared = self.convert_multipart(request).and_then(|request| {
            let (inner, _) = self.configured()?;
            Ok((inner.clone(), request))
        });
        async move {
            let (inner, request) = prepared?;
            let response = inner
                .send(request)
                .await
                .map_err(|error| http_client::Error::Instance(Box::new(error)))?;
            buffered_response(response)
        }
    }

    fn send_streaming<T>(
        &self,
        request: Request<T>,
    ) -> impl Future<Output = http_client::Result<http_client::StreamingResponse>> + Send
    where
        T: Into<Bytes> + Send,
    {
        let evidence = self.evidence.clone();
        let prepared = self.convert_request(request).and_then(|request| {
            let (inner, _) = self.configured()?;
            let lookup_headers = request.headers.clone();
            Ok((inner.clone(), request, lookup_headers))
        });
        async move {
            let (inner, request, lookup_headers) = prepared?;
            let response = inner
                .send_streaming(request)
                .await
                .map_err(|error| http_client::Error::Instance(Box::new(error)))?;
            streaming_response(response, inner, lookup_headers, evidence)
        }
    }
}

fn normalize_base_url(base_url: String) -> Result<String, RelayError> {
    let trimmed = base_url.trim_end_matches('/');
    if !(trimmed.starts_with("http://") || trimmed.starts_with("https://")) {
        return Err(RelayError::Build(
            "Relay base URL must be absolute http(s)".to_string(),
        ));
    }
    Ok(trimmed.to_string())
}

fn buffered_response<U>(response: HttpResponse) -> http_client::Result<Response<LazyBody<U>>>
where
    U: From<Bytes> + Send + 'static,
{
    let body = U::from(Bytes::from(response.body));
    let lazy: LazyBody<U> = Box::pin(async move { Ok(body) });
    build_response(response.status, response.headers, lazy)
}

fn streaming_response(
    response: StreamingHttpResponse,
    inner: Arc<PaidHttpClient>,
    lookup_headers: Vec<(String, String)>,
    evidence_state: EvidenceState,
) -> http_client::Result<http_client::StreamingResponse> {
    let request_id = response
        .header(aitp_client::codec::HEADER_REQUEST_ID)
        .map(str::to_string);
    let status = response.status;
    let headers = response.headers;
    let mut body = response.body;
    let mut reconciliation = StreamReconciliationGuard::new(
        inner,
        evidence_state,
        request_id.clone(),
        lookup_headers,
        (200..300).contains(&status),
    );

    let guarded = async_stream::stream! {
        let mut terminal = SseTerminalValidator::default();
        while let Some(chunk) = body.next().await {
            match chunk {
                Ok(chunk) => {
                    if let Err(message) = terminal.observe(&chunk) {
                        reconciliation.disarm();
                        yield Err(instance_error_owned(message));
                        return;
                    }
                    yield Ok(chunk);
                }
                Err(error) => {
                    reconciliation.disarm();
                    yield Err(http_client::Error::Instance(Box::new(error)));
                    return;
                }
            }
        }

        if !(200..300).contains(&status) {
            reconciliation.disarm();
            return;
        }
        if let Err(message) = terminal.finish() {
            reconciliation.disarm();
            yield Err(instance_error_owned(message));
            return;
        }
        if request_id.is_none() {
            yield Err(instance_error("paid Relay stream omitted AITP request id"));
            return;
        }
        match reconciliation.reconcile_now().await {
            Ok(Some(_)) => {}
            Ok(None) => {
                yield Err(instance_error("paid Relay stream has no durable settlement receipt"));
            }
            Err(error) => {
                yield Err(http_client::Error::Instance(Box::new(error)));
            }
        }
    };
    build_response(status, headers, Box::pin(guarded))
}

/// Owns the local evidence obligation for the lifetime of the consumer-facing
/// stream. Dropping the Rig stream drops this guard and transfers reconciliation
/// to a bounded Tokio task; without a runtime the pending id remains available
/// through the explicit recovery API.
struct StreamReconciliationGuard {
    inner: Arc<PaidHttpClient>,
    state: EvidenceState,
    request_id: Option<String>,
    armed: bool,
}

impl StreamReconciliationGuard {
    fn new(
        inner: Arc<PaidHttpClient>,
        state: EvidenceState,
        request_id: Option<String>,
        lookup_headers: Vec<(String, String)>,
        successful_status: bool,
    ) -> Self {
        let armed = successful_status && request_id.is_some();
        if let Some(request_id) = request_id.as_ref().filter(|_| armed) {
            state.register(request_id.clone(), lookup_headers);
        }
        Self {
            inner,
            state,
            request_id,
            armed,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    async fn reconcile_now(&mut self) -> Result<Option<PaymentEvidence>, aitp_client::PayError> {
        self.armed = false;
        let Some(request_id) = self.request_id.as_deref() else {
            return Ok(None);
        };
        let Some(headers) = self.state.begin(request_id) else {
            return Ok(None);
        };
        match self.inner.settlement_evidence(request_id, headers).await {
            Ok(evidence) if evidence.receipt.is_some() => {
                self.state.complete(request_id, evidence.clone());
                Ok(Some(evidence))
            }
            Ok(_) => {
                self.state.retryable(request_id);
                Ok(None)
            }
            Err(error) => {
                self.state.retryable(request_id);
                Err(error)
            }
        }
    }
}

impl Drop for StreamReconciliationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(request_id) = self.request_id.clone() else {
            return;
        };
        spawn_evidence_reconciliation(self.inner.clone(), self.state.clone(), request_id);
    }
}

fn spawn_evidence_reconciliation(
    inner: Arc<PaidHttpClient>,
    state: EvidenceState,
    request_id: String,
) {
    let Some(headers) = state.begin(&request_id) else {
        return;
    };
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        state.retryable(&request_id);
        return;
    };
    runtime.spawn(async move {
        // Relay may finish provider consumption and Mint settlement after the
        // caller stops polling. Keep this task bounded while retaining the id
        // for a later explicit retry if evidence never materializes.
        const MAX_ATTEMPTS: usize = 120;
        for attempt in 0..MAX_ATTEMPTS {
            match inner
                .settlement_evidence(&request_id, headers.clone())
                .await
            {
                Ok(evidence) if evidence.receipt.is_some() => {
                    state.complete(&request_id, evidence);
                    return;
                }
                Ok(_) if attempt + 1 < MAX_ATTEMPTS => {
                    tokio::time::sleep(Duration::from_millis(500)).await;
                }
                Ok(_) | Err(_) => break,
            }
        }
        state.retryable(&request_id);
    });
}

#[derive(Default)]
struct SseTerminalValidator {
    pending: Vec<u8>,
    saw_usage: bool,
}

impl SseTerminalValidator {
    const MAX_PENDING_EVENT_BYTES: usize = 1024 * 1024;

    fn observe(&mut self, chunk: &[u8]) -> Result<(), String> {
        self.pending.extend_from_slice(chunk);
        while let Some((index, delimiter_len)) = event_boundary(&self.pending) {
            let event = self.pending.drain(..index).collect::<Vec<_>>();
            self.pending.drain(..delimiter_len);
            self.observe_event(&event)?;
        }
        if self.pending.len() > Self::MAX_PENDING_EVENT_BYTES {
            return Err("Relay SSE event exceeded the validation limit".into());
        }
        Ok(())
    }

    fn finish(mut self) -> Result<(), String> {
        if self.pending.iter().any(|byte| !byte.is_ascii_whitespace()) {
            let remaining = std::mem::take(&mut self.pending);
            self.observe_event(&remaining)?;
        }
        if !self.saw_usage {
            return Err("paid Relay stream ended without trustworthy terminal usage".into());
        }
        Ok(())
    }

    fn observe_event(&mut self, event: &[u8]) -> Result<(), String> {
        let event = std::str::from_utf8(event)
            .map_err(|_| "Relay SSE event was not valid UTF-8".to_string())?;
        for line in event.lines() {
            let Some(data) = line.trim_start().strip_prefix("data:") else {
                continue;
            };
            let data = data.trim();
            if data.is_empty() || data == "[DONE]" {
                continue;
            }
            let Ok(value) = serde_json::from_str::<serde_json::Value>(data) else {
                continue;
            };
            if value.get("error").is_some_and(|error| !error.is_null()) {
                return Err("Relay SSE carried an application-level provider error".into());
            }
            if value.get("usage").is_some_and(valid_terminal_usage) {
                self.saw_usage = true;
            }
        }
        Ok(())
    }
}

fn valid_terminal_usage(usage: &serde_json::Value) -> bool {
    let input = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(serde_json::Value::as_u64);
    let output = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(serde_json::Value::as_u64);
    let total = usage
        .get("total_tokens")
        .and_then(serde_json::Value::as_u64);
    matches!((input, output, total), (Some(input), Some(output), Some(total)) if input.checked_add(output) == Some(total))
}

fn event_boundary(bytes: &[u8]) -> Option<(usize, usize)> {
    let lf = bytes
        .windows(2)
        .position(|window| window == b"\n\n")
        .map(|index| (index, 2));
    let crlf = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| (index, 4));
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(boundary), None) | (None, Some(boundary)) => Some(boundary),
        (None, None) => None,
    }
}

fn build_response<B>(
    status: u16,
    headers: Vec<(String, String)>,
    body: B,
) -> http_client::Result<Response<B>> {
    let mut builder = Response::builder().status(status);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    builder.body(body).map_err(http_client::Error::Protocol)
}

fn instance_error(message: &'static str) -> http_client::Error {
    http_client::Error::Instance(Box::new(std::io::Error::other(message)))
}

fn instance_error_owned(message: impl Into<String>) -> http_client::Error {
    http_client::Error::Instance(Box::new(std::io::Error::other(message.into())))
}
