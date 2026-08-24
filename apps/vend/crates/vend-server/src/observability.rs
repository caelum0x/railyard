//! Secret-safe HTTP telemetry for Vend's public and control planes.

use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{from_fn_with_state, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use tower_http::limit::RequestBodyLimitLayer;

use crate::security::RuntimeLimits;

const REQUEST_ID_HEADER: &str = "x-request-id";

/// Low-cardinality process metrics rendered in Prometheus' text exposition
/// format. No tenant, user, API-key, model, or URL values become labels.
#[derive(Debug, Default)]
pub struct Metrics {
    requests_total: AtomicU64,
    requests_in_flight: AtomicU64,
    request_duration_micros: AtomicU64,
    responses_2xx: AtomicU64,
    responses_4xx: AtomicU64,
    responses_5xx: AtomicU64,
    settlements_captured: AtomicU64,
    settlements_released: AtomicU64,
    settlements_failed: AtomicU64,
    request_sequence: AtomicU64,
}

impl Metrics {
    pub fn settlement_captured(&self) {
        self.settlements_captured.fetch_add(1, Ordering::Relaxed);
    }

    pub fn settlement_released(&self) {
        self.settlements_released.fetch_add(1, Ordering::Relaxed);
    }

    pub fn settlement_failed(&self) {
        self.settlements_failed.fetch_add(1, Ordering::Relaxed);
    }

    fn request_started(&self) -> InFlightGuard<'_> {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        self.requests_in_flight.fetch_add(1, Ordering::Relaxed);
        InFlightGuard { metrics: self }
    }

    fn request_finished(&self, status: StatusCode, elapsed_micros: u64) {
        self.request_duration_micros
            .fetch_add(elapsed_micros, Ordering::Relaxed);
        match status.as_u16() {
            200..=299 => &self.responses_2xx,
            400..=499 => &self.responses_4xx,
            500..=599 => &self.responses_5xx,
            _ => return,
        }
        .fetch_add(1, Ordering::Relaxed);
    }

    fn next_request_id(&self) -> String {
        let sequence = self.request_sequence.fetch_add(1, Ordering::Relaxed);
        let process = std::process::id();
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        format!("req_{nanos:032x}{process:08x}{sequence:016x}")
    }

    /// Prometheus text format without process-specific or customer labels.
    pub fn render(&self) -> String {
        let requests = self.requests_total.load(Ordering::Relaxed);
        let duration_seconds =
            self.request_duration_micros.load(Ordering::Relaxed) as f64 / 1_000_000.0;
        let mut out = String::with_capacity(1_024);
        family(
            &mut out,
            "vend_http_requests_total",
            "HTTP requests received by Vend.",
            "counter",
        );
        untyped_metric(&mut out, "vend_http_requests_total", requests);
        family(
            &mut out,
            "vend_http_requests_in_flight",
            "Vend HTTP requests currently executing.",
            "gauge",
        );
        untyped_metric(
            &mut out,
            "vend_http_requests_in_flight",
            self.requests_in_flight.load(Ordering::Relaxed),
        );
        family(
            &mut out,
            "vend_http_handler_duration_seconds",
            "Cumulative time until Vend produced response headers.",
            "summary",
        );
        let _ = writeln!(
            out,
            "vend_http_handler_duration_seconds_sum {duration_seconds:.6}"
        );
        untyped_metric(
            &mut out,
            "vend_http_handler_duration_seconds_count",
            requests,
        );
        family(
            &mut out,
            "vend_http_responses_total",
            "Vend HTTP responses by status class.",
            "counter",
        );
        labeled_metric(
            &mut out,
            "vend_http_responses_total",
            "class",
            "2xx",
            self.responses_2xx.load(Ordering::Relaxed),
        );
        labeled_metric(
            &mut out,
            "vend_http_responses_total",
            "class",
            "4xx",
            self.responses_4xx.load(Ordering::Relaxed),
        );
        labeled_metric(
            &mut out,
            "vend_http_responses_total",
            "class",
            "5xx",
            self.responses_5xx.load(Ordering::Relaxed),
        );
        family(
            &mut out,
            "vend_settlements_total",
            "Vend settlement terminal outcomes.",
            "counter",
        );
        labeled_metric(
            &mut out,
            "vend_settlements_total",
            "outcome",
            "captured",
            self.settlements_captured.load(Ordering::Relaxed),
        );
        labeled_metric(
            &mut out,
            "vend_settlements_total",
            "outcome",
            "released",
            self.settlements_released.load(Ordering::Relaxed),
        );
        labeled_metric(
            &mut out,
            "vend_settlements_total",
            "outcome",
            "failed",
            self.settlements_failed.load(Ordering::Relaxed),
        );
        out
    }
}

struct InFlightGuard<'a> {
    metrics: &'a Metrics,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.metrics
            .requests_in_flight
            .fetch_sub(1, Ordering::Relaxed);
    }
}

fn family(out: &mut String, name: &str, help: &str, metric_type: &str) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} {metric_type}");
}

fn untyped_metric(out: &mut String, name: &str, value: u64) {
    let _ = writeln!(out, "{name} {value}");
}

fn labeled_metric(out: &mut String, name: &str, label: &str, value: &str, count: u64) {
    let _ = writeln!(out, "{name}{{{label}=\"{value}\"}} {count}");
}

/// Middleware that emits one completion event and propagates a safe request id.
/// It deliberately logs only method, path (never query), status, and latency.
pub async fn trace_request(
    State(metrics): State<Arc<Metrics>>,
    mut request: Request,
    next: Next,
) -> Response {
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| valid_request_id(value))
        .map(str::to_owned)
        .unwrap_or_else(|| metrics.next_request_id());
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        request.headers_mut().insert(REQUEST_ID_HEADER, value);
    }

    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let started = Instant::now();
    let _guard = metrics.request_started();
    let mut response = next.run(request).await;
    let elapsed = started.elapsed();
    metrics.request_finished(
        response.status(),
        elapsed.as_micros().try_into().unwrap_or(u64::MAX),
    );
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, value);
    }
    tracing::info!(
        request_id,
        method = %method,
        path,
        status = response.status().as_u16(),
        latency_ms = elapsed.as_millis(),
        "Vend HTTP request completed"
    );
    response
}

fn valid_request_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

async fn render_metrics(metrics: Arc<Metrics>) -> impl IntoResponse {
    (
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        metrics.render(),
    )
}

/// Add bounded request bodies, low-cardinality metrics, and secret-safe request
/// tracing around a fully assembled Vend router. Tracing is the outer layer so
/// it also observes `413 Payload Too Large` responses from the ingress bound.
pub fn harden_router(app: Router, metrics: Arc<Metrics>, limits: RuntimeLimits) -> Router {
    let metrics_handler = metrics.clone();
    app.route(
        "/metrics",
        get(move || {
            let metrics = metrics_handler.clone();
            async move { render_metrics(metrics).await }
        }),
    )
    .layer(RequestBodyLimitLayer::new(limits.max_request_bytes))
    .layer(from_fn_with_state(metrics, trace_request))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use axum::routing::post;
    use tower::ServiceExt;

    #[test]
    fn request_ids_reject_log_injection() {
        assert!(valid_request_id("edge-01.trace_2"));
        assert!(!valid_request_id("edge-01\nforged=true"));
        assert!(!valid_request_id("contains space"));
        assert!(!valid_request_id(&"x".repeat(65)));
    }

    #[test]
    fn metrics_are_low_cardinality_prometheus_text() {
        let metrics = Metrics::default();
        metrics.settlement_captured();
        metrics.settlement_released();
        metrics.settlement_failed();
        let rendered = metrics.render();
        assert!(rendered.contains("vend_settlements_total{outcome=\"captured\"} 1"));
        assert!(rendered.contains("vend_settlements_total{outcome=\"released\"} 1"));
        assert!(rendered.contains("vend_settlements_total{outcome=\"failed\"} 1"));
        assert!(!rendered.contains("tenant"));
        assert!(!rendered.contains("api_key"));
    }

    #[tokio::test]
    async fn hardening_limits_bodies_and_propagates_request_ids() {
        let metrics = Arc::new(Metrics::default());
        let app = harden_router(
            Router::new().route("/echo", post(|body: String| async move { body })),
            metrics.clone(),
            RuntimeLimits::new(1_024).unwrap(),
        );
        let response = app
            .clone()
            .oneshot(
                HttpRequest::post("/echo")
                    .body(Body::from(vec![b'x'; 1_025]))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(response.headers().contains_key(REQUEST_ID_HEADER));

        let response = app
            .oneshot(
                HttpRequest::get("/metrics")
                    .header(REQUEST_ID_HEADER, "trusted-edge-01")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.headers()[REQUEST_ID_HEADER], "trusted-edge-01");
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let text = String::from_utf8(body.to_vec()).unwrap();
        assert!(text.contains("vend_http_responses_total{class=\"4xx\"} 1"));
    }
}
