//! Postpaid meter-only usage recording and per-key spend caps.
//!
//! Relay's prepaid path (`RELAY_BILLING_ENABLED`) emits a cost event only after
//! an AITP authorization and a Mint capture. Postpaid consumers (AutoGTM bills
//! its tenants through Rago) do not hold funds per request, so without this
//! module no usage would ever be recorded. With `RELAY_METERING_ENABLED=1` and
//! billing disabled, [`metering_middleware`] records every successful
//! authenticated inference as a durable [`CostEvent::metered`] priced at the
//! operator catalog's retail rate. The existing cost-feed worker then forwards
//! those events (e.g. to Rago), and the billing dashboard aggregates them.
//!
//! [`spend_cap_middleware`] enforces per-key monthly caps from
//! `relay_key_budgets` in BOTH modes, before any provider I/O.
//!
//! Metering is best-effort accounting, not a money path: an event append
//! failure is logged and never fails an already-served response. Streams are
//! metered from their terminal `usage` frame (the request is rewritten to ask
//! for one). A stream the client aborts before that frame is still metered from
//! an estimate (request bytes / 4 prompt tokens, streamed text chars / 4
//! completion tokens) because the provider already charged for the tokens it
//! produced.
//!
//! Provider batches (`POST /batch_inference`) are metered on completion: the
//! start records the owning key and retail selector in
//! `relay_metered_batches`, and the first poll that observes a `completed`
//! batch records one cost event per inference, keyed by the inference id so a
//! repeated poll can never double-bill. Recorded batch spend therefore counts
//! toward the owner's `relay_key_budgets` cap like synchronous spend.

use std::sync::Arc;

use axum::{
    Json,
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use futures::StreamExt;
use relay_billing::{ModelId, PriceTable, Usage};
use relay_contract::Microdollars;
use relay_cost_feed::CostFeedError;
use relay_cost_feed::{CostEvent, CostEventStore, InferenceRoute};
use relay_storage_postgres::{BudgetDecision, PostgresKeyBudgetStore, PostgresMeteredBatchStore};
use serde_json::json;
use tensorzero_auth::middleware::RequestApiKeyExtension;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use crate::billing::{
    EstimatePolicy, MAX_REQUEST_BODY_BYTES, MAX_RESPONSE_CAPTURE_BYTES,
    buffer_request_pricing_with_policy, event_has_error, extract_usage_from_event, is_batch_start,
    is_billable_path, is_event_stream, is_individual_batch_poll,
    parse_usage_and_model_with_fallback, read_u32, whole_batch_poll,
};
use crate::live_prices::LivePrices;

/// Rough characters-per-token ratio used only to estimate aborted streams.
const ESTIMATED_CHARS_PER_TOKEN: u32 = 4;

/// Whether postpaid meter-only recording is requested.
pub fn metering_enabled() -> bool {
    matches!(
        std::env::var("RELAY_METERING_ENABLED")
            .ok()
            .as_deref()
            .map(str::trim),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

/// Injected state for [`metering_middleware`].
#[derive(Clone)]
pub struct MeteringState {
    store: Arc<dyn CostEventStore>,
    prices: LivePrices,
    tasks: TaskTracker,
    batches: Option<PostgresMeteredBatchStore>,
}

impl MeteringState {
    pub fn new(store: Arc<dyn CostEventStore>, prices: LivePrices, tasks: TaskTracker) -> Self {
        Self {
            store,
            prices,
            tasks,
            batches: None,
        }
    }

    /// Enable batch metering with durable batch ownership.
    pub fn with_batches(mut self, batches: PostgresMeteredBatchStore) -> Self {
        self.batches = Some(batches);
        self
    }
}

/// Everything needed to persist one metered event once usage is known.
struct MeterSink {
    store: Arc<dyn CostEventStore>,
    tasks: TaskTracker,
    principal: String,
    route: InferenceRoute,
    request_id: Uuid,
}

impl MeterSink {
    fn record(self, usage: Usage, model: ModelId) {
        let event = CostEvent::metered(
            self.request_id,
            self.principal,
            model.as_str(),
            self.route,
            relay_contract::Usage {
                input_tokens: u64::from(usage.prompt_tokens),
                output_tokens: u64::from(usage.completion_tokens),
                total_tokens: u64::from(usage.total_tokens),
                provider_cost_micros: None,
            },
            Microdollars::new(usage.cost_micros.get().max(0)),
            relay_billing::now_ms(),
        );
        let store = self.store;
        self.tasks.spawn(async move {
            if let Err(error) = store.append(event).await {
                tracing::error!(error = %error, "Relay meter-only cost event append failed");
            }
        });
    }
}

/// Record successful authenticated inference as meter-only cost events.
pub async fn metering_middleware(
    State(state): State<MeteringState>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    let method = request.method().clone();
    let batch_start = is_batch_start(&method, &path);
    let batch_poll = whole_batch_poll(&method, &path)
        .and_then(Result::ok)
        .map(|batch_id| (batch_id, true))
        .or_else(|| {
            is_individual_batch_poll(&method, &path)
                .then(|| individual_poll_batch_id(&path))
                .flatten()
                .map(|batch_id| (batch_id, false))
        });
    if !batch_start && batch_poll.is_none() && !is_billable_path(&path) {
        return next.run(request).await;
    }
    let Some(principal) = request
        .extensions()
        .get::<RequestApiKeyExtension>()
        .map(|ext| ext.key_info.public_id.clone())
    else {
        return next.run(request).await;
    };
    if let Some((batch_id, whole)) = batch_poll {
        return meter_batch_poll(&state, request, next, principal, batch_id, whole).await;
    }
    let (request, pricing) =
        match buffer_request_pricing_with_policy(request, EstimatePolicy::default()).await {
            Ok(priced) => priced,
            Err(response) => return response,
        };
    let response = next.run(request).await;
    if !response.status().is_success() {
        return response;
    }
    if batch_start {
        return record_batch_start(&state, response, &principal, &pricing.model).await;
    }
    let sink = MeterSink {
        store: state.store.clone(),
        tasks: state.tasks.clone(),
        principal,
        route: pricing.route,
        request_id: Uuid::now_v7(),
    };
    if is_event_stream(&response) {
        let (parts, body) = response.into_parts();
        let request_bytes = pricing.request_bytes();
        let metered = MeteredStream {
            inner: http_body_util::BodyStream::new(body),
            pending: String::new(),
            usage: None,
            failed: false,
            prices: state.prices.current(),
            requested_model: pricing.model,
            request_bytes,
            streamed_chars: 0,
            sink: Some(sink),
        };
        return Response::from_parts(parts, Body::from_stream(metered));
    }

    let (parts, body) = response.into_parts();
    // The handler already materialized this JSON in memory; re-buffering it
    // cannot grow memory beyond one extra copy.
    let bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(error = %error, "Relay metering could not read the response body");
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": {"message": "failed to read inference response"}})),
            )
                .into_response();
        }
    };
    if bytes.len() <= MAX_RESPONSE_CAPTURE_BYTES {
        let parsed = serde_json::from_slice::<serde_json::Value>(&bytes).ok();
        let request_id = parsed
            .as_ref()
            .and_then(response_request_id)
            .unwrap_or(sink.request_id);
        if let Some((usage, model)) = parse_usage_and_model_with_fallback(
            &bytes,
            &state.prices.current(),
            Some(&pricing.model),
        ) {
            MeterSink { request_id, ..sink }.record(usage, model);
        } else {
            tracing::warn!(path = %path, "Relay metering found no usage in a successful response");
        }
    }
    Response::from_parts(parts, Body::from(bytes))
}

/// Prefer TensorZero's own inference id so metered events join observability.
fn response_request_id(value: &serde_json::Value) -> Option<Uuid> {
    value
        .get("inference_id")
        .or_else(|| value.get("id"))
        .and_then(serde_json::Value::as_str)
        .and_then(|raw| Uuid::parse_str(raw).ok())
}

/// SSE body wrapper that records the terminal usage frame when the stream ends.
struct MeteredStream {
    inner: http_body_util::BodyStream<Body>,
    pending: String,
    usage: Option<(Usage, ModelId)>,
    failed: bool,
    prices: Arc<PriceTable>,
    requested_model: ModelId,
    /// Request body size, the basis of the aborted-stream prompt estimate.
    request_bytes: u32,
    /// Generated text characters observed so far (aborted-stream estimate).
    streamed_chars: u64,
    sink: Option<MeterSink>,
}

impl MeteredStream {
    fn observe_event(&mut self, event: &str) {
        if event_has_error(event) {
            self.failed = true;
        }
        if let Some(found) = extract_usage_from_event(event, &self.prices, &self.requested_model) {
            self.usage = Some(found);
        }
        for line in event.lines() {
            let Some(payload) = line.strip_prefix("data:").map(str::trim) else {
                continue;
            };
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) {
                self.streamed_chars = self
                    .streamed_chars
                    .saturating_add(generated_text_chars(&value, None));
            }
        }
    }

    /// Usage estimate for a stream that ended before its terminal usage frame.
    fn estimated_usage(&self) -> Option<(Usage, ModelId)> {
        if self.streamed_chars == 0 {
            return None;
        }
        let prompt = self.request_bytes.div_ceil(ESTIMATED_CHARS_PER_TOKEN);
        let completion = u32::try_from(
            self.streamed_chars
                .div_ceil(u64::from(ESTIMATED_CHARS_PER_TOKEN)),
        )
        .unwrap_or(u32::MAX);
        let total = prompt.saturating_add(completion);
        let model = self.requested_model.clone();
        let cost = self.prices.cost(&model, prompt, completion);
        Some((Usage::new(prompt, completion, total, cost), model))
    }

    fn observe(&mut self, data: &[u8]) {
        self.pending.push_str(&String::from_utf8_lossy(data));
        while let Some(idx) = self.pending.find("\n\n") {
            let event: String = self.pending.drain(..idx + 2).collect();
            self.observe_event(&event);
        }
        if self.pending.len() > MAX_RESPONSE_CAPTURE_BYTES {
            self.pending.clear();
        }
    }

    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let event = std::mem::take(&mut self.pending);
        self.observe_event(&event);
    }
}

impl futures::Stream for MeteredStream {
    type Item = Result<axum::body::Bytes, axum::Error>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        match self.inner.poll_next_unpin(cx) {
            std::task::Poll::Ready(Some(Ok(frame))) => match frame.into_data() {
                Ok(data) => {
                    self.observe(&data);
                    std::task::Poll::Ready(Some(Ok(data)))
                }
                Err(_) => std::task::Poll::Ready(Some(Ok(axum::body::Bytes::new()))),
            },
            std::task::Poll::Ready(Some(Err(error))) => {
                self.failed = true;
                std::task::Poll::Ready(Some(Err(axum::Error::new(error))))
            }
            std::task::Poll::Ready(None) => {
                self.flush();
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

impl Drop for MeteredStream {
    fn drop(&mut self) {
        let Some(sink) = self.sink.take() else {
            return;
        };
        if self.failed {
            return;
        }
        if let Some((usage, model)) = self.usage.take() {
            sink.record(usage, model);
        } else if let Some((usage, model)) = self.estimated_usage() {
            // The client disconnected before the terminal usage frame. The
            // provider still produced (and billed) the streamed tokens, so an
            // estimate is recorded instead of letting the stream go free.
            tracing::info!(
                model = model.as_str(),
                prompt_tokens = usage.prompt_tokens,
                completion_tokens = usage.completion_tokens,
                "Relay metered an aborted stream from an estimate"
            );
            sink.record(usage, model);
        }
    }
}

/// Sum the generated text carried by one streamed chunk (OpenAI-compatible
/// `choices[].delta.content` / tool-call `arguments`, or native TensorZero
/// `content[].text` / `arguments`). Request echoes and metadata are ignored.
fn generated_text_chars(value: &serde_json::Value, key: Option<&str>) -> u64 {
    match value {
        serde_json::Value::String(text) => match key {
            Some("content" | "text" | "arguments" | "raw_arguments" | "raw_text") => {
                text.chars().count() as u64
            }
            _ => 0,
        },
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| generated_text_chars(item, key))
            .sum(),
        serde_json::Value::Object(map) => map
            .iter()
            .filter(|(name, _)| name.as_str() != "usage")
            .map(|(name, item)| generated_text_chars(item, Some(name.as_str())))
            .sum(),
        _ => 0,
    }
}

/// `.../batch_inference/{batch_id}/inference/{inference_id}` -> `batch_id`.
fn individual_poll_batch_id(path: &str) -> Option<Uuid> {
    let (_, suffix) = path.rsplit_once("/batch_inference/")?;
    let (batch, _) = suffix.split_once('/')?;
    Uuid::parse_str(batch).ok()
}

/// Persist ownership of a started meter-only batch so its completion is billed
/// to the key that started it.
async fn record_batch_start(
    state: &MeteringState,
    response: Response,
    principal: &str,
    model: &ModelId,
) -> Response {
    let Some(batches) = state.batches.as_ref() else {
        return response;
    };
    let (parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_RESPONSE_CAPTURE_BYTES).await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(error = %error, "Relay metering could not read a batch start response");
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": {"message": "failed to read batch start response"}})),
            )
                .into_response();
        }
    };
    let batch_id = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|value| value.get("batch_id")?.as_str().map(str::to_owned))
        .and_then(|raw| Uuid::parse_str(&raw).ok());
    match batch_id {
        Some(batch_id) => {
            if let Err(error) = batches
                .record_start(batch_id, principal, model.as_str())
                .await
            {
                // The batch is already running at the provider; failing the
                // response would only hide the batch id from its owner. The
                // completion poll falls back to billing the polling key.
                tracing::error!(error = %error, %batch_id, "Relay could not persist meter-only batch ownership");
            }
        }
        None => {
            tracing::warn!("Relay metering found no batch_id in a successful batch start response");
        }
    }
    Response::from_parts(parts, Body::from(bytes))
}

/// Meter a completed batch the first time a poll observes it.
async fn meter_batch_poll(
    state: &MeteringState,
    request: Request,
    next: Next,
    poller: String,
    batch_id: Uuid,
    whole_batch: bool,
) -> Response {
    let response = next.run(request).await;
    if !response.status().is_success() {
        return response;
    }
    let owner = match state.batches.as_ref() {
        Some(batches) => match batches.get(batch_id).await {
            Ok(record) => record,
            Err(error) => {
                tracing::error!(error = %error, %batch_id, "Relay meter-only batch lookup failed");
                None
            }
        },
        None => None,
    };
    if owner.as_ref().is_some_and(|record| record.metered) {
        return response;
    }
    let (parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_REQUEST_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(error = %error, %batch_id, "Relay metering could not read a batch poll response");
            return (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": {"message": "failed to read batch poll response"}})),
            )
                .into_response();
        }
    };
    let (principal, model) = match &owner {
        Some(record) => (
            record.owner_principal.clone(),
            ModelId::new(record.model.clone()),
        ),
        None => {
            tracing::warn!(%batch_id, "Relay metering has no ownership record for this batch; billing the polling key at the catalog default rate");
            (poller, ModelId::namespace("unknown"))
        }
    };
    let prices = state.prices.current();
    if let Some(completed) = parse_completed_batch(&bytes, &prices, &model, batch_id) {
        let store = state.store.clone();
        let batches = whole_batch.then(|| state.batches.clone()).flatten();
        state.tasks.spawn(async move {
            let mut all_recorded = true;
            for (inference_id, usage) in completed {
                let event = CostEvent::metered(
                    inference_id,
                    principal.clone(),
                    model.as_str(),
                    InferenceRoute::BatchInference,
                    relay_contract::Usage {
                        input_tokens: u64::from(usage.prompt_tokens),
                        output_tokens: u64::from(usage.completion_tokens),
                        total_tokens: u64::from(usage.total_tokens),
                        provider_cost_micros: None,
                    },
                    Microdollars::new(usage.cost_micros.get().max(0)),
                    relay_billing::now_ms(),
                );
                match store.append(event).await {
                    Ok(()) => {}
                    // The inference was already metered by an earlier poll.
                    Err(CostFeedError::Conflict) => {}
                    Err(error) => {
                        all_recorded = false;
                        tracing::error!(error = %error, %inference_id, %batch_id, "Relay meter-only batch cost event append failed");
                    }
                }
            }
            if all_recorded
                && let Some(batches) = batches
                && let Err(error) = batches.mark_metered(batch_id).await
            {
                tracing::warn!(error = %error, %batch_id, "Relay could not mark a batch as metered; later polls re-check idempotently");
            }
        });
    }
    Response::from_parts(parts, Body::from(bytes))
}

/// Per-inference usage of a completed batch poll, or `None` while pending,
/// failed, or unparseable.
fn parse_completed_batch(
    bytes: &[u8],
    prices: &PriceTable,
    model: &ModelId,
    expected_batch_id: Uuid,
) -> Option<Vec<(Uuid, Usage)>> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    if value.get("status")?.as_str()? != "completed" {
        return None;
    }
    let batch_id = value
        .get("batch_id")?
        .as_str()
        .and_then(|raw| Uuid::parse_str(raw).ok())?;
    if batch_id != expected_batch_id {
        return None;
    }
    let inferences = value.get("inferences")?.as_array()?;
    let mut completed = Vec::with_capacity(inferences.len());
    for inference in inferences {
        let Some(inference_id) = inference
            .get("inference_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|raw| Uuid::parse_str(raw).ok())
        else {
            continue;
        };
        let Some(usage) = inference.get("usage") else {
            continue;
        };
        let prompt = read_u32(usage, "input_tokens").unwrap_or(0);
        let completion = read_u32(usage, "output_tokens").unwrap_or(0);
        let total = prompt.saturating_add(completion);
        completed.push((
            inference_id,
            Usage::new(
                prompt,
                completion,
                total,
                prices.cost(model, prompt, completion),
            ),
        ));
    }
    Some(completed)
}

/// Injected state for [`spend_cap_middleware`].
#[derive(Clone)]
pub struct SpendCapState {
    budgets: PostgresKeyBudgetStore,
}

impl SpendCapState {
    pub fn new(budgets: PostgresKeyBudgetStore) -> Self {
        Self { budgets }
    }
}

/// Reject billable requests from keys that reached their monthly spend cap.
///
/// A budget lookup failure is logged and fails OPEN: caps protect against
/// runaway keys, and the durable billing path already fails closed on its own
/// Postgres dependency.
pub async fn spend_cap_middleware(
    State(state): State<SpendCapState>,
    request: Request,
    next: Next,
) -> Response {
    let path = request.uri().path();
    if !is_batch_start(request.method(), path) && !is_billable_path(path) {
        return next.run(request).await;
    }
    let Some(principal) = request
        .extensions()
        .get::<RequestApiKeyExtension>()
        .map(|ext| ext.key_info.public_id.clone())
    else {
        return next.run(request).await;
    };
    match state.budgets.check(&principal).await {
        Ok(BudgetDecision::Exceeded { limit, spent }) => spend_cap_exceeded(limit, spent),
        Ok(BudgetDecision::Unlimited | BudgetDecision::Within { .. }) => next.run(request).await,
        Err(error) => {
            tracing::error!(error = %error, "Relay spend-cap lookup failed; allowing request");
            next.run(request).await
        }
    }
}

fn spend_cap_exceeded(limit: Microdollars, spent: Microdollars) -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({
            "error": {
                "message": format!(
                    "monthly spend cap reached for this API key ({} of {} micro-USD used); raise the cap in Relay billing or wait for the next UTC month",
                    spent.get(),
                    limit.get()
                ),
                "type": "relay_spend_cap_exceeded",
                "code": "relay_spend_cap_exceeded",
                "limit_micros": limit.get(),
                "month_to_date_micros": spent.get(),
            }
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn response_request_id_prefers_inference_id() {
        let id = Uuid::now_v7();
        let value = json!({"inference_id": id.to_string(), "id": "chatcmpl-1"});
        assert_eq!(response_request_id(&value), Some(id));
        assert_eq!(response_request_id(&json!({"id": "chatcmpl-1"})), None);
    }

    #[test]
    fn completed_batch_meters_each_inference() {
        let batch = Uuid::now_v7();
        let first = Uuid::now_v7();
        let second = Uuid::now_v7();
        let body = json!({
            "batch_id": batch.to_string(),
            "status": "completed",
            "inferences": [
                {"inference_id": first.to_string(), "usage": {"input_tokens": 10, "output_tokens": 5}},
                {"inference_id": second.to_string(), "usage": {"input_tokens": 1, "output_tokens": 2}},
            ],
        });
        let prices = PriceTable::new(relay_billing::ModelPrice::new(1_000_000, 1_000_000));
        let model = ModelId::namespace("unknown");
        let parsed =
            parse_completed_batch(body.to_string().as_bytes(), &prices, &model, batch).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].0, first);
        assert_eq!(parsed[0].1.total_tokens, 15);
        assert!(
            parse_completed_batch(body.to_string().as_bytes(), &prices, &model, Uuid::now_v7())
                .is_none()
        );
        let pending = json!({"batch_id": batch.to_string(), "status": "pending"});
        assert!(
            parse_completed_batch(pending.to_string().as_bytes(), &prices, &model, batch).is_none()
        );
    }

    #[test]
    fn individual_poll_path_yields_batch_id() {
        let batch = Uuid::now_v7();
        let path = format!("/batch_inference/{batch}/inference/{}", Uuid::now_v7());
        assert_eq!(individual_poll_batch_id(&path), Some(batch));
    }

    #[test]
    fn generated_text_counts_only_output_fields() {
        let chunk = json!({
            "choices": [{"delta": {"role": "assistant", "content": "hello"}}],
            "model": "gpt-4o-mini",
            "usage": null,
        });
        assert_eq!(generated_text_chars(&chunk, None), 5);
        let native =
            json!({"inference_id": "x", "content": [{"type": "text", "id": "0", "text": "abc"}]});
        assert_eq!(generated_text_chars(&native, None), 3);
    }

    #[test]
    fn spend_cap_response_is_429_with_machine_code() {
        let response = spend_cap_exceeded(Microdollars::new(10), Microdollars::new(12));
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }
}
