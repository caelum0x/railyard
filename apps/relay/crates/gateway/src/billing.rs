//! Additive success-only billing hook for the inference route.
//!
//! This is the least-invasive graft of the clean-room `relay-billing` engine
//! onto the TensorZero gateway: a single tower/axum middleware layered *inside*
//! the auth layer (so the authenticated API-key identity is already in the
//! request extensions) and *scoped* to the inference-shaped routes. It performs
//! the hold -> capture/release lifecycle around the real inference handler
//! WITHOUT touching `tensorzero-core`:
//!
//! 1. HOLD: [`Ledger::reserve`] an estimate before the upstream call. On
//!    [`BillingError::InsufficientCredits`] we short-circuit `402 Payment
//!    Required` and never invoke the model.
//! 2. Run the real handler (`next.run`).
//! 3a. CAPTURE (success, non-streaming JSON 2xx): buffer the response body,
//!     parse the `usage` object, and [`Ledger::settle`] the EXACT actual cost
//!     in integer microdollars (ADR-0001) — never the estimate.
//! 3b. RELEASE (any non-2xx / transport failure): [`Ledger::release`] the hold
//!     so a failed job costs exactly zero (ADR-0002, success-only).
//! 3c. STREAMING (SSE 2xx): usage is not known when the handler returns, so we
//!     wrap the body in a [`StreamCarry`] Drop guard and parse the terminal
//!     `usage` frame out of the SSE stream as bytes flow to the client. On
//!     stream end we [`Ledger::settle`] the EXACT actual cost from the observed
//!     usage; if no usage frame was seen (e.g. the stream errored early) we
//!     [`Ledger::release`] the hold instead — so streaming success is billed and
//!     an aborted stream still costs zero, never overcharging.
//!
//! Idempotency is keyed on the minted [`JobId`], so a middleware retry can never
//! double-charge. The billing engine is injected as an `Arc<dyn BillingLedger>`
//! (the reserve/settle/release lifecycle seam): PRODUCTION wires a
//! [`relay_billing::MintLedger`] that holds/captures/releases on a real
//! mint-server over HTTP, while tests wire the in-memory `Ledger`
//! (`InMemoryStore`) or a `MintLedger` over a fake transport — with zero changes
//! to this middleware.

use std::sync::Arc;

use axum::{
    Json,
    body::Body,
    extract::{Request, State},
    http::StatusCode,
    middleware::Next,
    response::{IntoResponse, Response},
};
use relay_billing::{
    AccountId, BillingError, BillingLedger, JobId, Micros, ModelId, PriceTable, Reservation, Usage,
};
use serde_json::json;
use tensorzero_auth::middleware::RequestApiKeyExtension;

use crate::aitp::{
    AITP_CHALLENGE_HEADER, AITP_VERSION, AITP_VERSION_HEADER, Challenge, challenge_resource,
};

/// Maximum non-streaming response body we will buffer to read the `usage`
/// object (16 MiB). Larger bodies fall back to a release (no charge) rather
/// than risk unbounded buffering.
const MAX_CAPTURE_BODY_BYTES: usize = 16 * 1024 * 1024;

/// Default payee (provider receive address) advertised in AITP-402 challenges
/// when `RELAY_AITP_PAYEE` is unset. Deliberately a placeholder — the real
/// address is supplied via config/env at wiring time.
pub const DEFAULT_AITP_PAYEE: &str = "ait1qprovider";

/// The SAFE-DEFAULT per-token price applied to any model the [`PriceTable`] does
/// not know. Conservative (higher than most real rates) so an unpriced model is
/// never under-held or under-billed: `$0.000010`/prompt-token,
/// `$0.000030`/completion-token.
pub fn default_model_price() -> relay_billing::ModelPrice {
    relay_billing::ModelPrice::new(10, 30)
}

/// The hold-estimate policy. The HOLD placed before the upstream call is now
/// MODEL-AWARE: it prices an assumed token budget through the per-model
/// [`PriceTable`], rather than a flat model-agnostic amount. The final settled
/// cost is always the EXACT actual usage priced at the model's rate.
#[derive(Debug, Clone, Copy)]
pub struct EstimatePolicy {
    /// Assumed prompt/input tokens used to size the pre-call hold.
    pub hold_prompt_tokens: u32,
    /// Assumed completion/output tokens used to size the pre-call hold.
    pub hold_completion_tokens: u32,
}

impl Default for EstimatePolicy {
    fn default() -> Self {
        // A generous budget so the hold comfortably covers a typical request;
        // the exact actual usage is settled afterwards, so any over-hold is
        // released, never charged.
        Self {
            hold_prompt_tokens: 4_000,
            hold_completion_tokens: 4_000,
        }
    }
}

impl EstimatePolicy {
    /// The model-aware hold estimate: the assumed token budget priced at the
    /// requested model's rate (or the table's safe default for unknown models).
    fn hold_estimate(&self, prices: &PriceTable, model: &ModelId) -> Micros {
        prices.cost(model, self.hold_prompt_tokens, self.hold_completion_tokens)
    }
}

/// Injected axum state for [`billing_middleware`]. Cheap to clone (a couple of
/// `Arc`s and a `Copy` policy), as required by `from_fn_with_state`.
#[derive(Clone)]
pub struct BillingState {
    ledger: Arc<dyn BillingLedger>,
    policy: EstimatePolicy,
    /// Per-model microdollar price table (model-aware pricing seam).
    prices: Arc<PriceTable>,
    /// Provider receive address advertised in AITP-402 challenges.
    payee: Arc<str>,
}

impl BillingState {
    /// Construct billing state with a per-model [`PriceTable`] and the AITP
    /// payee address to advertise on `402` challenges.
    pub fn new(
        ledger: Arc<dyn BillingLedger>,
        policy: EstimatePolicy,
        prices: Arc<PriceTable>,
        payee: impl Into<Arc<str>>,
    ) -> Self {
        Self {
            ledger,
            policy,
            prices,
            payee: payee.into(),
        }
    }
}

/// The billing middleware. Layer it with
/// `middleware::from_fn_with_state(billing_state, billing_middleware)` INSIDE
/// the auth layer.
pub async fn billing_middleware(
    State(state): State<BillingState>,
    request: Request,
    next: Next,
) -> Response {
    // Only the inference-shaped routes are billable; everything else passes
    // straight through untouched.
    if !is_billable_path(request.uri().path()) {
        return next.run(request).await;
    }

    // The account identity comes from the authenticated API key that the auth
    // middleware placed in the request extensions. If it is absent (auth is
    // disabled, or this is an unauthenticated route), we do not bill.
    let Some(account) = account_from_request(&request) else {
        return next.run(request).await;
    };

    // Capture the request shape needed for MODEL-AWARE pricing and the AITP
    // challenge resource BEFORE consuming the request. The body is buffered so
    // we can read the requested `model`, then handed back to the handler
    // untouched.
    let method = request.method().as_str().to_string();
    let path = request.uri().path().to_string();
    let (request, model) = buffer_request_model(request).await;

    // Model-aware hold: price an assumed token budget at the requested model's
    // rate (safe default for unknown models) instead of a flat amount.
    let estimate = state.policy.hold_estimate(&state.prices, &model);

    let job = JobId::new();
    let reservation = match state.ledger.reserve(&account, estimate, job).await {
        Ok(reservation) => reservation,
        Err(BillingError::InsufficientCredits { .. }) => {
            // Short-circuit BEFORE ever calling the upstream model, emitting an
            // AITP-402 challenge (§5.1) so an agent can pay per request.
            let resource = challenge_resource(&method, &path, model.as_str());
            return aitp_payment_required(&state.payee, estimate, resource);
        }
        Err(other) => return billing_error_response(other),
    };

    // Arm an RAII guard over the whole reserve -> terminal window. If the inner
    // inference handler panics/unwinds while we `await` it below, `hold` is
    // dropped mid-unwind and releases the reservation, so a crashed request can
    // never leak a hold. We `disarm` it the instant the handler returns
    // normally, after which every terminal path (settle/release) is explicit.
    let mut hold = HoldGuard::new(state.ledger.clone(), reservation);

    let response = next.run(request).await;

    let Some(reservation) = hold.disarm() else {
        // Unreachable in practice: the guard stays armed until this line. If it
        // were ever already disarmed the hold is terminal, so just forward.
        return response;
    };

    // Any non-2xx status (or a transport failure surfaced as one) is a failed
    // job: release the hold in full so it costs exactly zero.
    if !response.status().is_success() {
        release_hold(&state.ledger, &reservation);
        return response;
    }

    if is_event_stream(&response) {
        // Streaming success: settle (or release) on stream end via the Drop guard.
        return attach_stream_carry(
            response,
            state.ledger.clone(),
            state.prices.clone(),
            reservation,
        );
    }

    // Non-streaming success: buffer, parse `usage`, settle the exact actual.
    capture_non_streaming(response, &state, reservation).await
}

/// Buffers a non-streaming 2xx response, parses its `usage`, and settles the
/// exact actual cost. On any buffering/parse failure we release the hold rather
/// than guess a charge.
async fn capture_non_streaming(
    response: Response,
    state: &BillingState,
    reservation: Reservation,
) -> Response {
    let (parts, body) = response.into_parts();

    // Never destroy a successful response in order to bill it. If the response
    // declares a Content-Length larger than we are willing to buffer, forward
    // the body untouched and release the hold (no charge) instead of consuming
    // it — buffering here would drop the client's data.
    if content_length_exceeds(&parts.headers, MAX_CAPTURE_BODY_BYTES) {
        release_hold(&state.ledger, &reservation);
        return Response::from_parts(parts, body);
    }

    let bytes = match axum::body::to_bytes(body, MAX_CAPTURE_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            // The body exceeded the cap while streaming (no/incorrect
            // Content-Length): we have already consumed it and cannot forward a
            // faithful copy. Release the hold (no charge) and surface an explicit
            // 502 rather than silently returning a truncated/empty 2xx body.
            release_hold(&state.ledger, &reservation);
            return capture_read_error();
        }
    };

    match parse_usage_and_model(&bytes, &state.prices) {
        Some((usage, model)) => {
            if let Err(err) = state.ledger.settle(&reservation, usage, model).await {
                // Settle failed (e.g. a transient store error). Do NOT leave the
                // hold dangling: release it as a terminal-idempotent fallback so
                // the reservation nets to zero instead of leaking the estimate.
                tracing::error!(error = %err, "billing settle failed; releasing hold as fallback");
                release_hold(&state.ledger, &reservation);
            }
        }
        None => {
            // No parseable usage -> a failed capture; release (costs nothing).
            release_hold(&state.ledger, &reservation);
        }
    }

    Response::from_parts(parts, Body::from(bytes))
}

/// RAII guard covering the reserve -> terminal window around the inference
/// handler. Armed at `reserve()`; on drop it releases the hold UNLESS it has
/// been `disarm`ed first. This makes the middleware panic-safe: if the handler
/// future unwinds, the guard drops and refunds the hold. Release is
/// terminal-idempotent, so a later explicit settle/release is unaffected.
struct HoldGuard {
    ledger: Arc<dyn BillingLedger>,
    reservation: Option<Reservation>,
}

impl HoldGuard {
    fn new(ledger: Arc<dyn BillingLedger>, reservation: Reservation) -> Self {
        Self {
            ledger,
            reservation: Some(reservation),
        }
    }

    /// Take the reservation out and disarm the guard (its drop becomes a no-op).
    fn disarm(&mut self) -> Option<Reservation> {
        self.reservation.take()
    }
}

impl Drop for HoldGuard {
    fn drop(&mut self) {
        if let Some(reservation) = self.reservation.take() {
            release_hold(&self.ledger, &reservation);
        }
    }
}

/// Wraps a streaming response body with a [`StreamCarry`] Drop guard. As bytes
/// flow to the client, [`GuardedBody`] parses the terminal SSE `usage` frame;
/// on stream end the guard settles the exact actual cost (or releases if no
/// usage was ever seen).
fn attach_stream_carry(
    response: Response,
    ledger: Arc<dyn BillingLedger>,
    prices: Arc<PriceTable>,
    reservation: Reservation,
) -> Response {
    let (parts, body) = response.into_parts();
    let carry = StreamCarry {
        ledger,
        reservation: Some(reservation),
        settled_usage: None,
    };
    // The guard is moved into the stream so it drops exactly when the body is
    // fully consumed or dropped (including on client disconnect, which the
    // gateway still drives to completion via `possibly_prevent_request_cancellation`).
    let guarded = Body::from_stream(GuardedBody::new(body, prices, carry));
    Response::from_parts(parts, guarded)
}

/// A Drop guard for a streamed job. On drop it applies the terminal action once:
/// settle the exact actual cost when a `usage` frame was observed on the stream,
/// otherwise release the hold (no usage seen -> never overcharge).
struct StreamCarry {
    ledger: Arc<dyn BillingLedger>,
    reservation: Option<Reservation>,
    /// The terminal usage parsed off the SSE stream, if any. `Some` -> settle
    /// the exact actual on drop; `None` -> release the hold.
    settled_usage: Option<(Usage, ModelId)>,
}

impl Drop for StreamCarry {
    fn drop(&mut self) {
        let Some(reservation) = self.reservation.take() else {
            return;
        };
        match self.settled_usage.take() {
            Some((usage, model)) => settle_hold(&self.ledger, &reservation, usage, model),
            None => release_hold(&self.ledger, &reservation),
        }
    }
}

/// A body wrapper that owns a [`StreamCarry`] alongside the inner byte stream,
/// so the guard drops precisely when the body is done. It also scans the SSE
/// frames it forwards for the terminal `usage` object used to settle the job.
struct GuardedBody {
    inner: http_body_util::BodyStream<Body>,
    prices: Arc<PriceTable>,
    /// SSE bytes not yet split on an event boundary (`\n\n`).
    pending: String,
    carry: StreamCarry,
}

impl GuardedBody {
    fn new(body: Body, prices: Arc<PriceTable>, carry: StreamCarry) -> Self {
        Self {
            inner: http_body_util::BodyStream::new(body),
            prices,
            pending: String::new(),
            carry,
        }
    }

    /// Feed freshly received SSE bytes through the event splitter, updating the
    /// carried terminal usage with the LAST parseable `usage` object seen.
    fn observe(&mut self, data: &[u8]) {
        // SSE payloads are UTF-8; a lossy decode is safe for scanning `data:`
        // frames (any replacement chars only appear inside forwarded bytes, which
        // we never mutate).
        self.pending.push_str(&String::from_utf8_lossy(data));
        while let Some(idx) = self.pending.find("\n\n") {
            let event: String = self.pending.drain(..idx + 2).collect();
            if let Some(found) = extract_usage_from_event(&event, &self.prices) {
                self.carry.settled_usage = Some(found);
            }
        }
        // Guard against unbounded growth if a stream never emits a boundary.
        if self.pending.len() > MAX_CAPTURE_BODY_BYTES {
            self.pending.clear();
        }
    }

    /// Flush any trailing event that lacked a final `\n\n` boundary.
    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let event = std::mem::take(&mut self.pending);
        if let Some(found) = extract_usage_from_event(&event, &self.prices) {
            self.carry.settled_usage = Some(found);
        }
    }
}

impl futures::Stream for GuardedBody {
    type Item = Result<axum::body::Bytes, axum::Error>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use futures::StreamExt;
        match self.inner.poll_next_unpin(cx) {
            std::task::Poll::Ready(Some(Ok(frame))) => {
                // Only forward data frames; trailers carry no bytes.
                if let Ok(data) = frame.into_data() {
                    self.observe(&data);
                    std::task::Poll::Ready(Some(Ok(data)))
                } else {
                    std::task::Poll::Ready(Some(Ok(axum::body::Bytes::new())))
                }
            }
            std::task::Poll::Ready(Some(Err(e))) => {
                std::task::Poll::Ready(Some(Err(axum::Error::new(e))))
            }
            std::task::Poll::Ready(None) => {
                self.flush();
                std::task::Poll::Ready(None)
            }
            std::task::Poll::Pending => std::task::Poll::Pending,
        }
    }
}

/// Parse the LAST `data:` line in an SSE event that carries a `usage` object
/// into a billed [`Usage`] plus [`ModelId`]. `[DONE]` sentinels and usage-less
/// frames yield `None`.
fn extract_usage_from_event(event: &str, prices: &PriceTable) -> Option<(Usage, ModelId)> {
    let mut last = None;
    for line in event.lines() {
        let Some(payload) = line.trim_start().strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        if let Some(found) = parse_usage_and_model(payload.as_bytes(), prices) {
            last = Some(found);
        }
    }
    last
}

/// Fire-and-forget settle for a streamed job. Idempotent on the ledger; on
/// failure it falls back to a terminal-idempotent release so the hold never
/// leaks.
fn settle_hold(
    ledger: &Arc<dyn BillingLedger>,
    reservation: &Reservation,
    usage: Usage,
    model: ModelId,
) {
    let ledger = ledger.clone();
    let reservation = reservation.clone();
    tokio::spawn(async move {
        if let Err(err) = ledger.settle(&reservation, usage, model).await {
            tracing::error!(error = %err, "streaming billing settle failed; releasing hold");
            let _ = ledger.release(&reservation).await;
        }
    });
}

/// Fire-and-forget release. The ledger release is idempotent, so spawning it is
/// safe even if a settle already ran (the first terminal action wins).
fn release_hold(ledger: &Arc<dyn BillingLedger>, reservation: &Reservation) {
    let ledger = ledger.clone();
    let reservation = reservation.clone();
    tokio::spawn(async move {
        let _ = ledger.release(&reservation).await;
    });
}

/// True for the inference-shaped routes we bill: the native `/inference` and the
/// OpenAI-compatible chat/completions/responses endpoints.
fn is_billable_path(path: &str) -> bool {
    path.ends_with("/inference")
        || path.ends_with("/chat/completions")
        || path.ends_with("/completions")
        || path.ends_with("/responses")
}

/// Resolve the billing [`AccountId`] from the authenticated API-key extension.
fn account_from_request(request: &Request) -> Option<AccountId> {
    request
        .extensions()
        .get::<RequestApiKeyExtension>()
        .map(|ext| AccountId::new(ext.key_info.public_id.clone()))
}

/// True when the response is a Server-Sent-Events stream.
fn is_event_stream(response: &Response) -> bool {
    response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.starts_with("text/event-stream"))
        .unwrap_or(false)
}

/// Parse a TensorZero / OpenAI-compatible response body into a billed
/// [`Usage`] plus the [`ModelId`] to label the debit. Returns `None` when the
/// body is not JSON or carries no usable `usage` object.
fn parse_usage_and_model(bytes: &[u8], prices: &PriceTable) -> Option<(Usage, ModelId)> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    let usage_obj = value.get("usage")?;
    // Streaming chunks commonly carry `"usage": null` until the terminal frame;
    // treat a null usage as "no usage" so we never settle a zero-cost placeholder.
    if usage_obj.is_null() {
        return None;
    }

    // TensorZero native uses input_tokens/output_tokens; OpenAI-compatible uses
    // prompt_tokens/completion_tokens. Accept either.
    let prompt =
        read_u32(usage_obj, "prompt_tokens").or_else(|| read_u32(usage_obj, "input_tokens"));
    let completion =
        read_u32(usage_obj, "completion_tokens").or_else(|| read_u32(usage_obj, "output_tokens"));
    let prompt = prompt.unwrap_or(0);
    let completion = completion.unwrap_or(0);
    let total = read_u32(usage_obj, "total_tokens").unwrap_or(0);

    let model = value
        .get("model")
        .and_then(|m| m.as_str())
        .or_else(|| value.get("model_name").and_then(|m| m.as_str()))
        .map(ModelId::namespace)
        .unwrap_or_else(|| ModelId::namespace("unknown"));

    // MODEL-AWARE cost: the exact actual usage priced at the served model's rate
    // (safe default for unknown models).
    let cost = prices.cost(&model, prompt, completion);
    let usage = Usage::new(prompt, completion, total, cost);

    Some((usage, model))
}

fn read_u32(obj: &serde_json::Value, key: &str) -> Option<u32> {
    obj.get(key)
        .and_then(|v| v.as_u64())
        .map(|n| n.min(u32::MAX as u64) as u32)
}

/// Buffer a billable request's body to read the requested `model`, then rebuild
/// the request with the same bytes so the downstream handler is unaffected.
/// Returns the (possibly-reconstructed) request and the namespaced [`ModelId`]
/// (falling back to `openrouter/unknown` when the body is missing/unparseable —
/// which the [`PriceTable`] prices at its safe default).
async fn buffer_request_model(request: Request) -> (Request, ModelId) {
    let unknown = ModelId::namespace("unknown");

    // Never buffer an oversized body just to read the model: forward untouched
    // and price at the default.
    if content_length_exceeds(request.headers(), MAX_CAPTURE_BODY_BYTES) {
        return (request, unknown);
    }

    let (parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_CAPTURE_BODY_BYTES).await {
        Ok(bytes) => bytes,
        // Body already partially consumed and over the cap — cannot faithfully
        // rebuild it, but we must return a request. Use an empty body; this only
        // happens for pathological oversized requests, which would fail parsing
        // downstream regardless.
        Err(_) => return (Request::from_parts(parts, Body::empty()), unknown),
    };

    let model = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .as_ref()
        .and_then(|v| v.get("model"))
        .and_then(|m| m.as_str())
        .map(ModelId::namespace)
        .unwrap_or(unknown);

    (Request::from_parts(parts, Body::from(bytes)), model)
}

/// Whether the success-only billing layer should be wired into the router.
///
/// Billing is OFF by default and must be explicitly opted into via
/// `RELAY_BILLING_ENABLED=1|true|yes`. When enabled, the router wires a real,
/// mint-server-backed [`relay_billing::MintLedger`] (see
/// `router.rs::build_billing_ledger`) that places holds, captures the exact
/// actual cost, and releases against a durable, funded ledger over HTTP — NOT
/// the in-memory `InMemoryStore` (that store is only used in tests). Enabling
/// billing is fail-closed: it requires `MINT_SERVER_URL`, `MINT_API_KEY`, and
/// `MINT_PAYEE_ACCOUNT`, and startup aborts with a config error if any is
/// missing, so billing is never silently run against an unconfigured backend.
pub fn billing_enabled() -> bool {
    matches!(
        std::env::var("RELAY_BILLING_ENABLED")
            .ok()
            .as_deref()
            .map(str::trim),
        Some("1") | Some("true") | Some("TRUE") | Some("yes") | Some("YES")
    )
}

/// Content-Length header (when present) exceeds `limit`.
fn content_length_exceeds(headers: &axum::http::HeaderMap, limit: usize) -> bool {
    headers
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .map(|len| len > limit)
        .unwrap_or(false)
}

/// `502 Bad Gateway` returned when a successful upstream body could not be
/// buffered to bill it and has already been partially consumed.
fn capture_read_error() -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "error": {
                "message": "response too large to process",
                "type": "billing_capture_error",
                "code": "billing_capture_error",
            }
        })),
    )
        .into_response()
}

/// `402 Payment Required` with an AITP-402 challenge (§5.1), returned before any
/// upstream call when the account cannot cover the model-aware hold. Carries the
/// `AITP-Version` and `AITP-Challenge` (base64url) headers alongside the existing
/// JSON error body, so both plain HTTP clients and AITP-aware agents are served.
fn aitp_payment_required(payee: &str, amount: Micros, resource: String) -> Response {
    let challenge = Challenge::mint_hold(payee, amount.get(), resource);
    let encoded = challenge.encode();

    let mut response = (
        StatusCode::PAYMENT_REQUIRED,
        Json(json!({
            "error": {
                "message": "insufficient credits",
                "type": "insufficient_credits",
                "code": "insufficient_credits",
            }
        })),
    )
        .into_response();

    let headers = response.headers_mut();
    if let Ok(value) = axum::http::HeaderValue::from_str(AITP_VERSION) {
        headers.insert(
            axum::http::HeaderName::from_static(AITP_VERSION_HEADER),
            value,
        );
    }
    if let Ok(value) = axum::http::HeaderValue::from_str(&encoded) {
        headers.insert(
            axum::http::HeaderName::from_static(AITP_CHALLENGE_HEADER),
            value,
        );
    }
    response
}

/// Map a non-credit billing engine failure onto an HTTP status at the edge.
fn billing_error_response(error: BillingError) -> Response {
    let (status, message) = match error {
        BillingError::InsufficientCredits { .. } => (
            StatusCode::PAYMENT_REQUIRED,
            "insufficient credits".to_string(),
        ),
        BillingError::InvalidAmount(m) => (StatusCode::BAD_REQUEST, m),
        BillingError::UnknownReservation(_) => {
            (StatusCode::BAD_REQUEST, "unknown reservation".to_string())
        }
        BillingError::Store(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "billing store error".to_string(),
        ),
    };
    (
        status,
        Json(json!({ "error": { "message": message, "type": "billing_error" } })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::Router;
    use axum::middleware as axum_middleware;
    use axum::routing::post;
    use relay_billing::MintLedger;
    use relay_billing::mint::fake::FakeMint;
    use relay_billing::{
        InMemoryStore, Ledger, LedgerEntry, LedgerKind, LedgerStore, ModelPrice, PriceTable,
    };
    use tensorzero_auth::key::TensorZeroApiKey;
    use tensorzero_auth::middleware::RequestApiKeyExtension;
    use tensorzero_auth::postgres::KeyInfo;
    use tower::ServiceExt;

    const ACCOUNT: &str = "acct_test01";
    const GPT4O_RAW: &str = "openai/gpt-4o";

    fn gpt4o() -> ModelId {
        ModelId::namespace(GPT4O_RAW)
    }

    /// Build a fake authenticated API-key extension whose `public_id` becomes the
    /// billing account id.
    fn key_extension(public_id: &str) -> RequestApiKeyExtension {
        RequestApiKeyExtension {
            api_key: Arc::new(TensorZeroApiKey {
                public_id: public_id.to_string(),
                hashed_long_key: secrecy::SecretString::from("hash".to_string()),
            }),
            key_info: KeyInfo {
                public_id: public_id.to_string(),
                organization: "org".to_string(),
                workspace: "ws".to_string(),
                description: None,
                created_at: chrono::Utc::now(),
                disabled_at: None,
                expires_at: None,
            },
        }
    }

    /// A price table: gpt-4o at 3µ$/prompt + 15µ$/completion, safe default for
    /// everything else (the fake catalog).
    fn price_table() -> Arc<PriceTable> {
        Arc::new(PriceTable::new(default_model_price()).with_model(gpt4o(), ModelPrice::new(3, 15)))
    }

    /// Ledger seeded with `micros` for [`ACCOUNT`]; returns the ledger and a
    /// handle to read the balance.
    async fn seeded_ledger(micros: i64) -> Arc<Ledger> {
        let store = InMemoryStore::new();
        if micros > 0 {
            store
                .append_ledger(vec![LedgerEntry::new(
                    AccountId::new(ACCOUNT),
                    LedgerKind::Deposit,
                    Micros::new(micros),
                    "seed",
                    None,
                )])
                .await
                .expect("seed deposit");
        }
        Arc::new(Ledger::new(Arc::new(store)))
    }

    async fn available(ledger: &Ledger) -> i64 {
        ledger
            .available_micros(&AccountId::new(ACCOUNT))
            .await
            .expect("balance")
            .get()
    }

    fn billing_state(ledger: Arc<Ledger>) -> BillingState {
        BillingState::new(
            ledger,
            EstimatePolicy::default(),
            price_table(),
            DEFAULT_AITP_PAYEE,
        )
    }

    /// A handler that echoes an OpenAI-compatible response with `model` + `usage`
    /// so the middleware can settle the exact model-aware cost.
    fn ok_response(model: &str, prompt: u32, completion: u32) -> Response {
        (
            StatusCode::OK,
            Json(json!({
                "model": model,
                "usage": {
                    "prompt_tokens": prompt,
                    "completion_tokens": completion,
                    "total_tokens": prompt + completion,
                }
            })),
        )
            .into_response()
    }

    /// Router: billing layer (inner) + an extension-injection layer (outer, runs
    /// first) so the authenticated identity is present when billing resolves it.
    fn app_with_account(
        state: BillingState,
        ext: RequestApiKeyExtension,
        handler_model: &str,
    ) -> Router {
        let handler_model = handler_model.to_string();
        Router::new()
            .route(
                "/v1/chat/completions",
                post(move || {
                    let m = handler_model.clone();
                    async move { ok_response(&m, 100, 50) }
                }),
            )
            .layer(axum_middleware::from_fn_with_state(
                state,
                billing_middleware,
            ))
            .layer(axum_middleware::from_fn(
                move |mut req: Request, next: Next| {
                    let ext = ext.clone();
                    async move {
                        req.extensions_mut().insert(ext);
                        next.run(req).await
                    }
                },
            ))
    }

    /// Billing state whose engine is a Mint-backed [`MintLedger`] over a fake
    /// transport (no live mint-server), so the middleware drives real
    /// hold/capture/release RPCs we can assert against.
    fn mint_billing_state(ledger: Arc<dyn BillingLedger>) -> BillingState {
        BillingState::new(
            ledger,
            EstimatePolicy::default(),
            price_table(),
            DEFAULT_AITP_PAYEE,
        )
    }

    /// A [`MintLedger`] over `fake`, capturing to `acct_treasury`.
    fn mint_ledger(fake: Arc<FakeMint>) -> Arc<dyn BillingLedger> {
        Arc::new(MintLedger::new(fake, "mint-key", "acct_treasury", None))
    }

    /// Router whose inference handler always returns `status` (to exercise the
    /// failure/release path), wired with the billing + identity layers.
    fn app_with_status(
        state: BillingState,
        ext: RequestApiKeyExtension,
        status: StatusCode,
    ) -> Router {
        Router::new()
            .route(
                "/v1/chat/completions",
                post(move || async move {
                    (status, Json(json!({ "error": "boom" }))).into_response()
                }),
            )
            .layer(axum_middleware::from_fn_with_state(
                state,
                billing_middleware,
            ))
            .layer(axum_middleware::from_fn(
                move |mut req: Request, next: Next| {
                    let ext = ext.clone();
                    async move {
                        req.extensions_mut().insert(ext);
                        next.run(req).await
                    }
                },
            ))
    }

    fn chat_request(model: &str) -> Request {
        Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "model": model, "messages": [] }).to_string(),
            ))
            .expect("request")
    }

    // ---- model-aware settle (funded) ------------------------------------

    #[tokio::test]
    async fn funded_request_settles_exact_model_aware_cost() {
        let ledger = seeded_ledger(1_000_000).await;
        let app = app_with_account(
            billing_state(ledger.clone()),
            key_extension(ACCOUNT),
            GPT4O_RAW,
        );

        let response = app
            .oneshot(chat_request(GPT4O_RAW))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);

        // gpt-4o: 100*3 + 50*15 = 300 + 750 = 1050 µ$ debited exactly.
        assert_eq!(available(&ledger).await, 1_000_000 - 1_050);
    }

    #[tokio::test]
    async fn unknown_model_settles_at_the_safe_default_rate() {
        let ledger = seeded_ledger(1_000_000).await;
        let unknown = "mystery/model";
        let app = app_with_account(
            billing_state(ledger.clone()),
            key_extension(ACCOUNT),
            unknown,
        );

        let response = app.oneshot(chat_request(unknown)).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK);

        // default: 100*10 + 50*30 = 1000 + 1500 = 2500 µ$.
        assert_eq!(available(&ledger).await, 1_000_000 - 2_500);
    }

    // ---- AITP-402 challenge (insufficient funds) ------------------------

    #[tokio::test]
    async fn insufficient_funds_returns_well_formed_aitp_402_challenge() {
        let ledger = seeded_ledger(0).await; // unfunded
        let app = app_with_account(
            billing_state(ledger.clone()),
            key_extension(ACCOUNT),
            GPT4O_RAW,
        );

        let response = app
            .oneshot(chat_request(GPT4O_RAW))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);

        assert_eq!(
            response
                .headers()
                .get(crate::aitp::AITP_VERSION_HEADER)
                .and_then(|v| v.to_str().ok()),
            Some(crate::aitp::AITP_VERSION)
        );

        let encoded = response
            .headers()
            .get(crate::aitp::AITP_CHALLENGE_HEADER)
            .and_then(|v| v.to_str().ok())
            .expect("challenge header present");
        let challenge = crate::aitp::Challenge::decode(encoded).expect("challenge decodes");

        assert_eq!(challenge.scheme, "aitp-hold");
        assert_eq!(challenge.currency, "AIT");
        assert_eq!(challenge.payee, DEFAULT_AITP_PAYEE);
        // Model-aware hold: default budget (4000/4000) at gpt-4o rate (3/15).
        assert_eq!(challenge.amount_micros, 4000 * 3 + 4000 * 15);
        assert_eq!(
            challenge.resource,
            format!("POST /v1/chat/completions#{}", gpt4o().as_str())
        );
        assert!(!challenge.nonce.is_empty(), "nonce must be present");
        assert!(challenge.expiry > 0, "expiry must be set");
    }

    // ---- Mint-backed lifecycle (real hold/capture/release RPCs) ---------

    #[tokio::test]
    async fn funded_request_holds_then_captures_on_mint() {
        let fake = Arc::new(FakeMint::funded(100_000_000));
        let app = app_with_account(
            mint_billing_state(mint_ledger(fake.clone())),
            key_extension(ACCOUNT),
            GPT4O_RAW,
        );

        let response = app
            .oneshot(chat_request(GPT4O_RAW))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);

        // HOLD placed with the model-aware estimate (4000 prompt/4000 completion
        // at gpt-4o's 3/15 rate = 12_000 + 60_000 = 72_000) from this account.
        let holds = fake.matching("POST", "/v1/holds");
        assert_eq!(holds.len(), 1, "exactly one hold created");
        let hold_body: serde_json::Value =
            serde_json::from_slice(&holds[0].body).expect("hold body json");
        assert_eq!(hold_body["from"], ACCOUNT);
        assert_eq!(hold_body["amount_micros"], 72_000);

        // CAPTURE at the EXACT actual (100*3 + 50*15 = 1050) to the payee — mint
        // auto-releases the remainder, so the net debit is the actual.
        let caps = fake.matching("POST", "/v1/holds/hold_01FAKE/capture");
        assert_eq!(caps.len(), 1, "exactly one capture");
        let cap_body: serde_json::Value =
            serde_json::from_slice(&caps[0].body).expect("capture body json");
        assert_eq!(cap_body["amount_micros"], 1_050);
        assert_eq!(cap_body["to"], "acct_treasury");
    }

    #[tokio::test]
    async fn failed_request_releases_the_mint_hold_and_never_captures() {
        let fake = Arc::new(FakeMint::funded(100_000_000));
        let app = app_with_status(
            mint_billing_state(mint_ledger(fake.clone())),
            key_extension(ACCOUNT),
            StatusCode::INTERNAL_SERVER_ERROR,
        );

        let response = app
            .oneshot(chat_request(GPT4O_RAW))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);

        // The hold was placed, then RELEASED (release is fire-and-forget/spawned).
        assert_eq!(fake.matching("POST", "/v1/holds").len(), 1);
        let released = fake
            .wait_for(
                "POST",
                "/v1/holds/hold_01FAKE/release",
                std::time::Duration::from_secs(2),
            )
            .await;
        assert!(released.is_some(), "hold released on failure");
        // A failed job costs zero — never captured.
        assert!(
            fake.matching("POST", "/v1/holds/hold_01FAKE/capture")
                .is_empty(),
            "failed job must not capture"
        );
    }

    #[tokio::test]
    async fn insufficient_funds_on_mint_returns_402_and_never_captures() {
        let fake = Arc::new(FakeMint::insufficient());
        let app = app_with_account(
            mint_billing_state(mint_ledger(fake.clone())),
            key_extension(ACCOUNT),
            GPT4O_RAW,
        );

        let response = app
            .oneshot(chat_request(GPT4O_RAW))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);

        // Exactly one hold attempt; the model is never invoked, nothing captured.
        assert_eq!(fake.matching("POST", "/v1/holds").len(), 1);
        assert!(
            fake.matching("POST", "/v1/holds/hold_01FAKE/capture")
                .is_empty(),
            "no capture when the hold was refused"
        );
    }

    // ---- unaffected paths ----------------------------------------------

    #[tokio::test]
    async fn unauthenticated_request_is_not_billed() {
        // No extension-injection layer => no account => pure pass-through even
        // against an unfunded ledger (billing effectively disabled).
        let ledger = seeded_ledger(0).await;
        let state = billing_state(ledger.clone());
        let app = Router::new()
            .route(
                "/v1/chat/completions",
                post(|| async { ok_response(GPT4O_RAW, 100, 50) }),
            )
            .layer(axum_middleware::from_fn_with_state(
                state,
                billing_middleware,
            ));

        let response = app
            .oneshot(chat_request(GPT4O_RAW))
            .await
            .expect("response");
        // Passes through untouched: no 402, and nothing debited.
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(available(&ledger).await, 0);
    }

    #[tokio::test]
    async fn non_billable_path_passes_through_untouched() {
        let ledger = seeded_ledger(0).await;
        let app = Router::new()
            .route(
                "/health",
                post(|| async { (StatusCode::OK, "ok").into_response() }),
            )
            .layer(axum_middleware::from_fn_with_state(
                billing_state(ledger.clone()),
                billing_middleware,
            ));

        let request = Request::builder()
            .method("POST")
            .uri("/health")
            .body(Body::empty())
            .expect("request");
        let response = app.oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(available(&ledger).await, 0);
    }

    #[tokio::test]
    async fn billing_disabled_by_default_env() {
        // The safety gate stays OFF unless explicitly opted in.
        assert!(!billing_enabled() || std::env::var("RELAY_BILLING_ENABLED").is_ok());
    }

    // ---- unit: request/response model extraction -------------------------

    #[tokio::test]
    async fn buffer_request_model_extracts_namespaced_model() {
        let request = chat_request(GPT4O_RAW);
        let (rebuilt, model) = buffer_request_model(request).await;
        assert_eq!(model, gpt4o());
        // Body preserved for the downstream handler.
        let bytes = axum::body::to_bytes(rebuilt.into_body(), MAX_CAPTURE_BODY_BYTES)
            .await
            .expect("body");
        assert!(!bytes.is_empty());
    }

    #[test]
    fn parse_usage_prices_response_with_model_aware_rate() {
        let prices = price_table();
        let body = json!({
            "model": GPT4O_RAW,
            "usage": { "prompt_tokens": 100, "completion_tokens": 50 }
        })
        .to_string();
        let (usage, model) = parse_usage_and_model(body.as_bytes(), &prices).expect("usage parsed");
        assert_eq!(model, gpt4o());
        assert_eq!(usage.cost_micros, Micros::new(1_050));
    }
}
