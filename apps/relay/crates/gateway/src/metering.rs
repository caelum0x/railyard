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
//! for one); a stream aborted before that frame is not metered.

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
use relay_cost_feed::{CostEvent, CostEventStore, InferenceRoute};
use relay_storage_postgres::{BudgetDecision, PostgresKeyBudgetStore};
use serde_json::json;
use tensorzero_auth::middleware::RequestApiKeyExtension;
use tokio_util::task::TaskTracker;
use uuid::Uuid;

use crate::billing::{
    EstimatePolicy, MAX_RESPONSE_CAPTURE_BYTES, buffer_request_pricing_with_policy,
    event_has_error, extract_usage_from_event, is_batch_start, is_billable_path, is_event_stream,
    parse_usage_and_model_with_fallback,
};

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
    prices: Arc<PriceTable>,
    tasks: TaskTracker,
}

impl MeteringState {
    pub fn new(
        store: Arc<dyn CostEventStore>,
        prices: Arc<PriceTable>,
        tasks: TaskTracker,
    ) -> Self {
        Self {
            store,
            prices,
            tasks,
        }
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
    // Batch jobs complete asynchronously; only synchronous inference is metered.
    if is_batch_start(request.method(), &path) || !is_billable_path(&path) {
        return next.run(request).await;
    }
    let Some(principal) = request
        .extensions()
        .get::<RequestApiKeyExtension>()
        .map(|ext| ext.key_info.public_id.clone())
    else {
        return next.run(request).await;
    };
    let (request, pricing) =
        match buffer_request_pricing_with_policy(request, EstimatePolicy::default()).await {
            Ok(priced) => priced,
            Err(response) => return response,
        };
    let response = next.run(request).await;
    if !response.status().is_success() {
        return response;
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
        let metered = MeteredStream {
            inner: http_body_util::BodyStream::new(body),
            pending: String::new(),
            usage: None,
            failed: false,
            prices: state.prices.clone(),
            requested_model: pricing.model,
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
        if let Some((usage, model)) =
            parse_usage_and_model_with_fallback(&bytes, &state.prices, Some(&pricing.model))
        {
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
        }
    }
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
    fn spend_cap_response_is_429_with_machine_code() {
        let response = spend_cap_exceeded(Microdollars::new(10), Microdollars::new(12));
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    }
}
