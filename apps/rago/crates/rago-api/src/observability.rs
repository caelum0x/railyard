//! Observability — a dependency-free Prometheus metrics registry, an axum
//! middleware that records per-request counts/latency + emits a tracing span,
//! a `/metrics` text-exposition endpoint, and a simple fixed-window rate
//! limiter middleware.
//!
//! Matches Lago's "Prometheus metrics for APIs/queues/workers/webhooks" story:
//! HTTP request/latency counters plus domain counters (invoices generated,
//! emails sent, webhooks enqueued, usage alerts fired, jobs processed) that the
//! handlers bump directly via [`incr`].

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

/// A single monotonically-increasing counter.
#[derive(Default)]
struct Counter(AtomicU64);

/// The process-wide metrics registry.
struct Registry {
    /// Named domain/HTTP counters (`rago_<name>_total`).
    counters: Mutex<BTreeMap<String, u64>>,
    /// Total requests observed (for the latency average).
    http_requests: Counter,
    /// Cumulative request latency in microseconds (for the average).
    http_latency_us: Counter,
    /// Requests currently in flight.
    http_in_flight: AtomicU64,
    /// Latency histogram buckets (upper bound ms → count), cumulative-friendly.
    latency_buckets: Mutex<BTreeMap<u64, u64>>,
}

impl Registry {
    fn new() -> Self {
        Self {
            counters: Mutex::new(BTreeMap::new()),
            http_requests: Counter::default(),
            http_latency_us: Counter::default(),
            http_in_flight: AtomicU64::new(0),
            latency_buckets: Mutex::new(BTreeMap::new()),
        }
    }
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();

fn registry() -> &'static Registry {
    REGISTRY.get_or_init(Registry::new)
}

/// The Prometheus latency histogram bucket bounds (milliseconds).
const BUCKETS_MS: &[u64] = &[5, 10, 25, 50, 100, 250, 500, 1000, 2500, 5000];

/// Increment a named domain counter (`rago_<name>_total`) by one.
pub fn incr(name: &str) {
    incr_by(name, 1);
}

/// Increment a named domain counter by `n`.
pub fn incr_by(name: &str, n: u64) {
    let mut map = registry().counters.lock().unwrap();
    *map.entry(name.to_string()).or_insert(0) += n;
}

fn record_request(latency_ms: u64) {
    let r = registry();
    r.http_requests.0.fetch_add(1, Ordering::Relaxed);
    r.http_latency_us.0.fetch_add(latency_ms.saturating_mul(1000), Ordering::Relaxed);
    let mut buckets = r.latency_buckets.lock().unwrap();
    for &b in BUCKETS_MS {
        if latency_ms <= b {
            *buckets.entry(b).or_insert(0) += 1;
        }
    }
}

/// Axum middleware: count the request, time it, emit a tracing span, and bump
/// the in-flight gauge.
pub async fn track_metrics(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let span = tracing::info_span!("http_request", %method, path = %path);
    let _guard = span.enter();

    let r = registry();
    r.http_in_flight.fetch_add(1, Ordering::Relaxed);
    let start = Instant::now();
    let response = next.run(req).await;
    let elapsed_ms = start.elapsed().as_millis() as u64;
    r.http_in_flight.fetch_sub(1, Ordering::Relaxed);
    record_request(elapsed_ms);
    incr_by(&format!("http_response_{}", response.status().as_u16() / 100 * 100), 1);

    tracing::info!(status = response.status().as_u16(), latency_ms = elapsed_ms, "request completed");
    response
}

/// `GET /metrics` — Prometheus text exposition format.
pub async fn metrics_handler() -> Response {
    let r = registry();
    let requests = r.http_requests.0.load(Ordering::Relaxed);
    let latency_us = r.http_latency_us.0.load(Ordering::Relaxed);
    let in_flight = r.http_in_flight.load(Ordering::Relaxed);

    let mut out = String::new();
    out.push_str("# HELP rago_http_requests_total Total HTTP requests handled.\n");
    out.push_str("# TYPE rago_http_requests_total counter\n");
    out.push_str(&format!("rago_http_requests_total {requests}\n"));

    out.push_str("# HELP rago_http_requests_in_flight In-flight HTTP requests.\n");
    out.push_str("# TYPE rago_http_requests_in_flight gauge\n");
    out.push_str(&format!("rago_http_requests_in_flight {in_flight}\n"));

    out.push_str("# HELP rago_http_request_duration_seconds HTTP request latency.\n");
    out.push_str("# TYPE rago_http_request_duration_seconds histogram\n");
    {
        let buckets = r.latency_buckets.lock().unwrap();
        for &b in BUCKETS_MS {
            let count = buckets.get(&b).copied().unwrap_or(0);
            out.push_str(&format!(
                "rago_http_request_duration_seconds_bucket{{le=\"{:.3}\"}} {}\n",
                b as f64 / 1000.0,
                count
            ));
        }
        out.push_str(&format!("rago_http_request_duration_seconds_bucket{{le=\"+Inf\"}} {requests}\n"));
        out.push_str(&format!("rago_http_request_duration_seconds_sum {:.6}\n", latency_us as f64 / 1_000_000.0));
        out.push_str(&format!("rago_http_request_duration_seconds_count {requests}\n"));
    }

    // Domain counters.
    let counters = r.counters.lock().unwrap();
    for (name, value) in counters.iter() {
        let metric = format!("rago_{name}_total");
        out.push_str(&format!("# TYPE {metric} counter\n"));
        out.push_str(&format!("{metric} {value}\n"));
    }

    (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/plain; version=0.0.4")], out).into_response()
}

// ---- rate limiting ---------------------------------------------------------

struct RateLimiter {
    /// Max requests per window.
    limit: u64,
    /// Window length in seconds.
    window_secs: u64,
    /// (window_start_epoch, count_in_window).
    state: Mutex<(u64, u64)>,
}

static RATE_LIMITER: OnceLock<RateLimiter> = OnceLock::new();

fn rate_limiter() -> &'static RateLimiter {
    RATE_LIMITER.get_or_init(|| {
        let limit = std::env::var("RAGO_RATE_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(6000);
        RateLimiter {
            limit,
            window_secs: 60,
            state: Mutex::new((now_epoch(), 0)),
        }
    })
}

fn now_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A fixed-window request budget per caller credential, so one organization's
/// burst (an ingest loop, a runaway client) cannot spend the budget every
/// other organization shares. Keys are hashes of the credential, never the
/// credential itself; the map only holds the current window.
pub struct PerKeyLimiter {
    limit: u64,
    window_secs: u64,
    state: Mutex<(u64, std::collections::HashMap<u64, u64>)>,
}

impl PerKeyLimiter {
    pub fn new(limit: u64, window_secs: u64) -> Self {
        Self { limit, window_secs: window_secs.max(1), state: Mutex::new((0, std::collections::HashMap::new())) }
    }

    /// Count one request of `key` at `now`; `Err(retry_after_secs)` when over.
    pub fn check(&self, key: u64, now: u64) -> Result<(), u64> {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let window = now - now % self.window_secs;
        if state.0 != window {
            *state = (window, std::collections::HashMap::new());
        }
        let count = state.1.entry(key).or_insert(0);
        *count += 1;
        if *count <= self.limit {
            Ok(())
        } else {
            Err((window + self.window_secs).saturating_sub(now).max(1))
        }
    }
}

static KEY_LIMITER: OnceLock<PerKeyLimiter> = OnceLock::new();

fn key_limiter() -> &'static PerKeyLimiter {
    KEY_LIMITER.get_or_init(|| {
        // Per credential (an organization's API key, or a session): default
        // half of the global budget, so no single caller can take all of it.
        let limit = std::env::var("RAGO_KEY_RATE_LIMIT").ok().and_then(|v| v.parse().ok()).unwrap_or(3000);
        PerKeyLimiter::new(limit, 60)
    })
}

/// The bucket of a request: its bearer credential, else its client address.
fn caller_key(req: &Request) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match req.headers().get(axum::http::header::AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        Some(auth) if !auth.trim().is_empty() => ("auth", auth.trim()).hash(&mut h),
        _ => {
            let peer = req.extensions().get::<axum::extract::ConnectInfo<std::net::SocketAddr>>().map(|c| c.0);
            ("anon", crate::client_ip::client_ip(req.headers(), peer).map(|ip| ip.to_string()).unwrap_or_default()).hash(&mut h)
        }
    }
    h.finish()
}

/// Axum middleware: a per-credential budget (`RAGO_KEY_RATE_LIMIT`, default
/// 3000/min) and the fixed-window global backstop (`RAGO_RATE_LIMIT`, default
/// 6000/min). Over either: `429` with `Retry-After`.
pub async fn rate_limit(req: Request, next: Next) -> Response {
    if let Err(retry) = key_limiter().check(caller_key(&req), now_epoch()) {
        incr("rate_limited_key");
        return (
            StatusCode::TOO_MANY_REQUESTS,
            [(axum::http::header::RETRY_AFTER, retry.to_string())],
            "rate limit exceeded for this API key; retry later",
        )
            .into_response();
    }
    let rl = rate_limiter();
    let now = now_epoch();
    let allowed = {
        let mut state = rl.state.lock().unwrap();
        if now.saturating_sub(state.0) >= rl.window_secs {
            *state = (now, 0);
        }
        state.1 += 1;
        state.1 <= rl.limit
    };
    if !allowed {
        incr("rate_limited");
        return (StatusCode::TOO_MANY_REQUESTS, [(axum::http::header::RETRY_AFTER, "60")], "rate limit exceeded").into_response();
    }
    next.run(req).await
}

#[cfg(test)]
mod key_limit_tests {
    use super::*;

    #[test]
    fn one_key_over_budget_does_not_block_another() {
        let l = PerKeyLimiter::new(3, 60);
        for _ in 0..3 {
            assert!(l.check(1, 1_000).is_ok());
        }
        assert_eq!(l.check(1, 1_010), Err(10), "retry when the window turns (1_020)");
        assert!(l.check(2, 1_010).is_ok(), "another organization still gets through");
        // A new window starts every key over.
        assert!(l.check(1, 1_020).is_ok());
    }
}
