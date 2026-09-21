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
//!   parse the `usage` object, and [`Ledger::settle`] the EXACT actual cost
//!   in integer microdollars (ADR-0001) — never the estimate.
//! 3b. RELEASE (any non-2xx / transport failure): [`Ledger::release`] the hold
//!   so a failed job costs exactly zero (ADR-0002, success-only).
//! 3c. STREAMING (SSE 2xx): usage is not known when the handler returns, so we
//!   wrap the body in a [`StreamCarry`] Drop guard and parse the terminal
//!   `usage` frame out of the SSE stream as bytes flow to the client. On
//!   stream end we [`Ledger::settle`] the EXACT actual cost from the observed
//!   usage; if no usage frame was seen (e.g. the stream errored early) we
//!   [`Ledger::release`] the hold instead — so streaming success is billed and
//!   an aborted stream still costs zero, never overcharging.
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
    http::{Method, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use relay_aitp::{
    ChallengeIssuer, ChallengePolicy, InMemoryChallengeStore, SystemClock, UuidNonceSource,
};
use relay_billing::{
    AccountId, BillingError, BillingLedger, InMemorySettlementEvidenceStore, JobId, Micros,
    ModelId, PriceTable, Reservation, SettlementEvidence, SettlementEvidenceStore, Usage,
};
use relay_contract::{AitpChallenge, Microdollars};
use relay_cost_feed::InferenceRoute;
use serde_json::json;
use tensorzero_auth::middleware::RequestApiKeyExtension;
use tokio_util::task::TaskTracker;

#[cfg(test)]
use crate::aitp::Challenge;
use crate::aitp::{
    AITP_AUTHORIZATION_HEADER, AITP_CHALLENGE_HEADER, AITP_RECEIPT_HEADER, AITP_REQUEST_ID_HEADER,
    AITP_VERSION, AITP_VERSION_HEADER, challenge_resource,
};
use crate::settlement_outbox::{BatchClaim, DurableBillingCoordinator};

/// Maximum non-streaming response body we will buffer to read the `usage`
/// object (16 MiB). Larger bodies fall back to a release (no charge) rather
/// than risk unbounded buffering.
const MAX_RESPONSE_CAPTURE_BYTES: usize = 16 * 1024 * 1024;

/// Maximum request body accepted by the gateway and therefore by request
/// pricing. Keep this identical to the router's `DefaultBodyLimit`: paid
/// multimodal requests must not become unpriceable merely because they exceed
/// the smaller response-capture ceiling.
pub(crate) const MAX_REQUEST_BODY_BYTES: usize = 100 * 1024 * 1024;

/// Default payee (provider receive address) advertised in AITP-402 challenges
/// when `RELAY_AITP_PAYEE` is unset. Deliberately a placeholder — the real
/// address is supplied via config/env at wiring time.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "test fallback constant is not used in production wiring"
    )
)]
pub const DEFAULT_AITP_PAYEE: &str = "ait1qprovider";

/// The safe-default per-million-token price applied to any model the
/// [`PriceTable`] does not know. This matches the highest input/output rates in
/// the embedded catalog, so an unresolved TensorZero function cannot select a
/// built-in model that costs more than its hold: `$15/M` prompt and `$75/M`
/// completion. An operator catalog may explicitly replace this default.
pub fn default_model_price() -> relay_billing::ModelPrice {
    relay_billing::ModelPrice::new(15_000_000, 75_000_000)
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
    fn hold_estimate(self, prices: &PriceTable, request: &RequestPricing) -> Micros {
        let prompt_tokens = self.hold_prompt_tokens.max(request.prompt_token_ceiling);
        let completion_tokens = request
            .completion_token_ceiling
            .unwrap_or(self.hold_completion_tokens);
        prices.cost(&request.model, prompt_tokens, completion_tokens)
    }
}

/// Request-derived inputs used to size the authorization before inference.
#[derive(Debug, Clone)]
struct RequestPricing {
    model: ModelId,
    route: InferenceRoute,
    /// A conservative prompt ceiling derived from the request byte length. A
    /// tokenizer cannot emit more tokens than source bytes for valid UTF-8 JSON.
    prompt_token_ceiling: u32,
    /// Client-authorized output maximum (`max_tokens` family), when supplied.
    completion_token_ceiling: Option<u32>,
}

/// Injected axum state for [`billing_middleware`]. Cheap to clone (a couple of
/// `Arc`s and a `Copy` policy), as required by `from_fn_with_state`.
#[derive(Clone)]
pub struct BillingState {
    coordinator: Arc<DurableBillingCoordinator>,
    evidence: Arc<dyn SettlementEvidenceStore>,
    policy: EstimatePolicy,
    /// Per-model microdollar price table (model-aware pricing seam).
    prices: Arc<PriceTable>,
    /// Provider receive address advertised in AITP-402 challenges.
    payee: Arc<str>,
    challenges: Arc<ChallengeIssuer>,
    /// Owns refund tasks created by synchronous `Drop` guards so graceful
    /// shutdown waits for their durable terminal command.
    deferred_tasks: TaskTracker,
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
        let payee = payee.into();
        let challenges = Arc::new(ChallengeIssuer::new(
            ChallengePolicy {
                payee: payee.to_string(),
                ttl_ms: crate::aitp::AITP_CHALLENGE_TTL_MS,
                max_amount_micros: Microdollars::new(i64::MAX),
            },
            Arc::new(InMemoryChallengeStore::default()),
            Arc::new(SystemClock),
            Arc::new(UuidNonceSource),
        ));
        Self {
            coordinator: Arc::new(DurableBillingCoordinator::in_memory(ledger.clone())),
            evidence: Arc::new(InMemorySettlementEvidenceStore::default()),
            policy,
            prices,
            payee,
            challenges,
            deferred_tasks: TaskTracker::new(),
        }
    }

    /// Replace the test/development evidence log with a durable implementation.
    pub fn with_evidence_store(mut self, evidence: Arc<dyn SettlementEvidenceStore>) -> Self {
        self.evidence = evidence;
        self
    }

    /// Install the Postgres-backed coordinator used in production.
    pub fn with_coordinator(mut self, coordinator: Arc<DurableBillingCoordinator>) -> Self {
        self.coordinator = coordinator;
        self
    }

    pub fn with_challenge_issuer(mut self, challenges: Arc<ChallengeIssuer>) -> Self {
        self.challenges = challenges;
        self
    }

    /// Use the gateway's tracked task set for panic/disconnect refunds.
    pub fn with_deferred_tasks(mut self, deferred_tasks: TaskTracker) -> Self {
        self.deferred_tasks = deferred_tasks;
        self
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
    if let Some(request_id) = settlement_lookup_request(request.method(), request.uri().path()) {
        let Some(principal) = account_from_request(&request) else {
            return settlement_lookup_error(
                StatusCode::UNAUTHORIZED,
                "authenticated principal required",
            );
        };
        return match request_id {
            Ok(request_id) => match state.evidence.get(&principal, &request_id).await {
                Ok(Some(evidence)) => Json(json!({ "data": evidence })).into_response(),
                Ok(None) => {
                    settlement_lookup_error(StatusCode::NOT_FOUND, "settlement evidence not found")
                }
                Err(error) => {
                    tracing::error!(error = %error, "settlement evidence lookup failed");
                    settlement_lookup_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "settlement evidence unavailable",
                    )
                }
            },
            Err(()) => {
                settlement_lookup_error(StatusCode::BAD_REQUEST, "invalid settlement request id")
            }
        };
    }

    let batch_start = is_batch_start(request.method(), request.uri().path());
    if let Some(batch_id) = whole_batch_poll(request.method(), request.uri().path()) {
        let Some(account) = account_from_request(&request) else {
            return next.run(request).await;
        };
        return match batch_id {
            Ok(batch_id) => handle_batch_poll(&state, request, next, account, batch_id).await,
            Err(()) => batch_lookup_error(StatusCode::BAD_REQUEST, "invalid batch id"),
        };
    }
    if is_individual_batch_poll(request.method(), request.uri().path()) {
        // A partial response cannot prove aggregate usage for the one hold.
        // Paid callers reconcile atomically through the whole-batch endpoint.
        return batch_lookup_error(
            StatusCode::BAD_REQUEST,
            "paid batches must be polled through /batch_inference/{batch_id}",
        );
    }

    // Only the inference-shaped routes are billable; everything else passes
    // straight through untouched.
    if !batch_start && !is_billable_path(request.uri().path()) {
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
    let (request, request_pricing) =
        match buffer_request_pricing_with_policy(request, state.policy).await {
            Ok(priced) => priced,
            Err(response) => return response,
        };
    // TensorZero functions can select different provider models at runtime.
    // A generic fallback is therefore not an authoritative retail price and
    // may under-hold an expensive variant or overcharge a cheap one. Require
    // the operator to publish an explicit function price before provider I/O.
    if request_pricing.model.requires_explicit_price()
        && !state.prices.is_known(&request_pricing.model)
    {
        return billing_price_unavailable(&request_pricing.model);
    }
    let authorization = request
        .headers()
        .get(AITP_AUTHORIZATION_HEADER)
        .and_then(|value| value.to_str().ok());
    // Only a paid retry may resume a server-issued request id. Ordinary funded
    // requests always get a fresh job, so a caller cannot reuse a correlation
    // header to collapse two successful requests onto one hold.
    let job = authorization
        .and_then(|_| request.headers().get(AITP_REQUEST_ID_HEADER))
        .and_then(|value| value.to_str().ok())
        .and_then(JobId::parse)
        .unwrap_or_default();

    // A caller may retry after Relay captured successfully but its response was
    // lost. We cannot reproduce the provider body, but we can return the
    // principal-scoped terminal evidence without reserving, executing, or
    // charging again. This turns an ambiguous timeout into a deterministic
    // recovery response instead of a generic duplicate-execution failure.
    if authorization.is_some() {
        match state.evidence.get(&account, &job).await {
            Ok(Some(evidence)) => {
                return attach_request_id(already_settled_response(&evidence), job);
            }
            Ok(None) => {}
            Err(error) => {
                tracing::error!(error = %error, request_id = %job, "settlement replay lookup failed");
                return attach_request_id(
                    settlement_lookup_error(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "settlement evidence unavailable",
                    ),
                    job,
                );
            }
        }
    }

    // Model-aware hold: price an assumed token budget at the requested model's
    // rate (safe default for unknown models) instead of a flat amount.
    let estimate = state.policy.hold_estimate(&state.prices, &request_pricing);
    // Bind correlation into the payer-signed resource. A copied authorization
    // cannot be replayed under a different request id even before Mint's durable
    // nonce replay guard runs.
    let resource = format!(
        "{}@{}",
        challenge_resource(&method, &path, request_pricing.model.as_str()),
        job
    );
    let reserve_result = if let Some(encoded) = authorization {
        match relay_billing::aitp_client::codec::decode_authorization(encoded) {
            Ok(auth) => match validate_authorization(&auth, &state, estimate, &resource) {
                Ok(()) => match state
                    .challenges
                    .authorize(account.as_str(), job.as_uuid(), &auth)
                    .await
                {
                    Ok(_) => {
                        state
                            .coordinator
                            .reserve_authorized(&account, &auth, estimate, job)
                            .await
                    }
                    Err(error) => {
                        return attach_request_id(
                            aitp_authorization_error(&error.to_string()),
                            job,
                        );
                    }
                },
                Err(message) => return attach_request_id(aitp_authorization_error(&message), job),
            },
            Err(error) => {
                return attach_request_id(aitp_authorization_error(&error.to_string()), job);
            }
        }
    } else {
        state.coordinator.reserve(&account, estimate, job).await
    };
    let reservation = match reserve_result {
        Ok(reservation) => reservation,
        Err(BillingError::InsufficientCredits { .. }) => {
            // Short-circuit BEFORE ever calling the upstream model, emitting an
            // AITP-402 challenge (§5.1) so an agent can pay per request.
            let challenge = match state
                .challenges
                .issue(
                    account.as_str(),
                    job.as_uuid(),
                    Microdollars::new(estimate.get()),
                    resource,
                )
                .await
            {
                Ok(challenge) => challenge,
                Err(error) => {
                    tracing::error!(error = %error, request_id = %job, "failed to persist AITP challenge");
                    return attach_request_id(
                        settlement_lookup_error(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "payment challenge unavailable",
                        ),
                        job,
                    );
                }
            };
            return attach_request_id(aitp_payment_required(&challenge), job);
        }
        Err(other) => return attach_request_id(billing_error_response(other), job),
    };

    // Arm an RAII guard over the whole reserve -> terminal window. If the inner
    // inference handler panics/unwinds while we `await` it below, `hold` is
    // dropped mid-unwind and releases the reservation, so a crashed request can
    // never leak a hold. We `disarm` it the instant the handler returns
    // normally, after which every terminal path (settle/release) is explicit.
    if let Err(error) = state.coordinator.mark_inference_started(&reservation).await {
        // This caller never acquired execution ownership. Releasing here could
        // select `release` for a concurrent winner's live hold.
        return attach_request_id(billing_error_response(error), job);
    }
    let mut hold = HoldGuard::new(
        state.coordinator.clone(),
        state.deferred_tasks.clone(),
        reservation,
    );

    let response = next.run(request).await;

    let Some(reservation) = hold.disarm() else {
        // Unreachable in practice: the guard stays armed until this line. If it
        // were ever already disarmed the hold is terminal, so just forward.
        return attach_request_id(response, job);
    };

    // Any non-2xx status (or a transport failure surfaced as one) is a failed
    // job: release the hold in full so it costs exactly zero.
    if !response.status().is_success() {
        release_hold(&state.coordinator, &reservation).await;
        return attach_request_id(response, job);
    }

    if is_event_stream(&response) {
        // Streaming success: settle (or release) on stream end via the Drop guard.
        return attach_request_id(
            attach_stream_carry(
                response,
                state.coordinator.clone(),
                state.evidence.clone(),
                state.prices.clone(),
                state.deferred_tasks.clone(),
                reservation,
                request_pricing.model,
                request_pricing.route,
            ),
            job,
        );
    }

    if batch_start {
        return attach_request_id(
            capture_batch_start(response, &state, reservation, request_pricing.model).await,
            job,
        );
    }

    // Non-streaming success: buffer, parse `usage`, settle the exact actual.
    attach_request_id(
        capture_non_streaming(response, &state, reservation, request_pricing.route).await,
        job,
    )
}

fn attach_request_id(mut response: Response, job: JobId) -> Response {
    if let Ok(value) = axum::http::HeaderValue::from_str(&job.to_string()) {
        response.headers_mut().insert(
            axum::http::HeaderName::from_static(AITP_REQUEST_ID_HEADER),
            value,
        );
    }
    response
}

fn validate_authorization(
    auth: &relay_billing::aitp_client::PaymentAuthorization,
    state: &BillingState,
    estimate: Micros,
    resource: &str,
) -> Result<(), String> {
    use relay_billing::aitp_client::Scheme;

    if auth.payee != state.payee.as_ref() {
        return Err("authorization payee does not match Relay".to_string());
    }
    if auth.resource != resource {
        return Err("authorization resource does not match request".to_string());
    }
    if auth.amount_micros.get() < estimate.get() {
        return Err("authorization amount is below the required reservation".to_string());
    }
    if auth.expiry <= relay_billing::now_ms() {
        return Err("authorization is expired".to_string());
    }
    if auth.scheme != Scheme::Hold {
        return Err("Relay metered inference requires aitp-hold".to_string());
    }
    Ok(())
}

/// Buffers a non-streaming 2xx response, parses its `usage`, and settles the
/// exact actual cost. A response is never delivered as a free 2xx when billing
/// cannot observe or settle its usage.
async fn capture_non_streaming(
    response: Response,
    state: &BillingState,
    reservation: Reservation,
    route: InferenceRoute,
) -> Response {
    let (mut parts, body) = response.into_parts();

    // A response too large to meter cannot be delivered as an unbilled success.
    // Release because the client receives a billing failure, not the model body.
    if content_length_exceeds(&parts.headers, MAX_RESPONSE_CAPTURE_BYTES) {
        release_hold(&state.coordinator, &reservation).await;
        return capture_read_error();
    }

    let bytes = match axum::body::to_bytes(body, MAX_RESPONSE_CAPTURE_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            // The body exceeded the cap while streaming (no/incorrect
            // Content-Length): we have already consumed it and cannot forward a
            // faithful copy. Release the hold (no charge) and surface an explicit
            // 502 rather than silently returning a truncated/empty 2xx body.
            release_hold(&state.coordinator, &reservation).await;
            return capture_read_error();
        }
    };

    match parse_usage_and_model(&bytes, &state.prices) {
        Some((usage, model)) => {
            let (usage, capped) = cap_usage_to_reservation(usage, &reservation);
            let settlement = match state
                .coordinator
                .settle(&reservation, usage, model, route)
                .await
            {
                Ok(settlement) => settlement,
                Err(err) => {
                    // The upstream succeeded, so releasing would turn a paid model
                    // call into a free one. Keep the authorization open for recovery
                    // and fail the client response explicitly.
                    tracing::error!(error = %err, "billing settle failed; retaining hold for recovery");
                    return billing_settle_error();
                }
            };
            if let Some(receipt) = settlement.receipt.clone() {
                if let Err(error) =
                    record_evidence(&state.evidence, &reservation, &settlement).await
                {
                    tracing::error!(error = %error, "billing settled but evidence persistence failed");
                    return billing_evidence_error();
                }
                if let Ok(encoded) = relay_billing::aitp_client::codec::encode_receipt(&receipt)
                    && let Ok(value) = axum::http::HeaderValue::from_str(&encoded)
                {
                    parts.headers.insert(
                        axum::http::HeaderName::from_static(AITP_RECEIPT_HEADER),
                        value,
                    );
                }
                parts.headers.insert(
                    axum::http::HeaderName::from_static(AITP_VERSION_HEADER),
                    axum::http::HeaderValue::from_static(AITP_VERSION),
                );
            }
            if capped {
                parts.headers.insert(
                    "x-relay-billing-overage",
                    axum::http::HeaderValue::from_static("capped-to-reservation"),
                );
            }
        }
        None => {
            // A successful response without trustworthy usage must never be
            // delivered for free. The client receives a billing failure, so the
            // undelivered model body is released at zero cost.
            release_hold(&state.coordinator, &reservation).await;
            return billing_settle_error();
        }
    }

    Response::from_parts(parts, Body::from(bytes))
}

/// Persist the asynchronous provider batch identity while the original request
/// still owns the execution lease. The accepted body is exposed only after the
/// durable mapping exists.
async fn capture_batch_start(
    response: Response,
    state: &BillingState,
    reservation: Reservation,
    model: ModelId,
) -> Response {
    let (parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_RESPONSE_CAPTURE_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            release_hold(&state.coordinator, &reservation).await;
            return capture_read_error();
        }
    };
    let batch_id = serde_json::from_slice::<serde_json::Value>(&bytes)
        .ok()
        .and_then(|value| value.get("batch_id")?.as_str().map(str::to_owned))
        .and_then(|raw| uuid::Uuid::parse_str(&raw).ok());
    let Some(batch_id) = batch_id else {
        release_hold(&state.coordinator, &reservation).await;
        return batch_state_error("batch start response did not contain a valid batch_id");
    };
    if let Err(error) = state
        .coordinator
        .mark_batch_pending(&reservation, batch_id, &model)
        .await
    {
        tracing::error!(error = %error, %batch_id, "failed to persist paid batch ownership");
        release_hold(&state.coordinator, &reservation).await;
        return batch_state_error("paid batch ownership could not be persisted");
    }
    Response::from_parts(parts, Body::from(bytes))
}

async fn handle_batch_poll(
    state: &BillingState,
    request: Request,
    next: Next,
    account: AccountId,
    batch_id: uuid::Uuid,
) -> Response {
    let claim = match state.coordinator.claim_batch(&account, batch_id).await {
        Ok(claim) => claim,
        Err(error) => {
            tracing::error!(error = %error, %batch_id, "failed to claim paid batch poll");
            return batch_state_error("paid batch state is unavailable");
        }
    };
    let (reservation, model) = match claim {
        BatchClaim::Acquired { reservation, model } => (reservation, model),
        BatchClaim::Busy => {
            let mut response = batch_lookup_error(
                StatusCode::CONFLICT,
                "another request is already polling this batch",
            );
            response.headers_mut().insert(
                axum::http::header::RETRY_AFTER,
                axum::http::HeaderValue::from_static("1"),
            );
            return response;
        }
        BatchClaim::NotFound => {
            return batch_lookup_error(StatusCode::NOT_FOUND, "paid batch not found");
        }
        BatchClaim::Settling => {
            return batch_lookup_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "batch settlement is still being committed",
            );
        }
        BatchClaim::Terminal => return next.run(request).await,
    };

    let response = next.run(request).await;
    if !response.status().is_success() {
        release_batch_poll_lease(&state.coordinator, &reservation).await;
        return attach_request_id(response, reservation.job_id);
    }
    let job = reservation.job_id;
    attach_request_id(
        capture_batch_poll(response, state, reservation, model, batch_id).await,
        job,
    )
}

enum BatchPollCapture {
    Pending,
    Failed,
    Completed(Usage),
}

async fn capture_batch_poll(
    response: Response,
    state: &BillingState,
    reservation: Reservation,
    model: ModelId,
    expected_batch_id: uuid::Uuid,
) -> Response {
    let (mut parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_REQUEST_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            release_batch_poll_lease(&state.coordinator, &reservation).await;
            return capture_read_error();
        }
    };
    let capture = parse_batch_poll_capture(&bytes, &state.prices, &model, expected_batch_id);
    match capture {
        Some(BatchPollCapture::Pending) => {
            release_batch_poll_lease(&state.coordinator, &reservation).await;
        }
        Some(BatchPollCapture::Failed) => {
            if let Err(error) = state.coordinator.release(&reservation).await {
                tracing::error!(error = %error, %expected_batch_id, "failed to release failed paid batch");
                return billing_settle_error();
            }
        }
        Some(BatchPollCapture::Completed(usage)) => {
            let (usage, capped) = cap_usage_to_reservation(usage, &reservation);
            let settlement = match state
                .coordinator
                .settle(&reservation, usage, model, InferenceRoute::BatchInference)
                .await
            {
                Ok(settlement) => settlement,
                Err(error) => {
                    tracing::error!(error = %error, %expected_batch_id, "paid batch settlement failed");
                    return billing_settle_error();
                }
            };
            if let Some(receipt) = settlement.receipt.clone() {
                if let Err(error) =
                    record_evidence(&state.evidence, &reservation, &settlement).await
                {
                    tracing::error!(error = %error, "paid batch evidence persistence failed");
                    return billing_evidence_error();
                }
                if let Ok(encoded) = relay_billing::aitp_client::codec::encode_receipt(&receipt)
                    && let Ok(value) = axum::http::HeaderValue::from_str(&encoded)
                {
                    parts.headers.insert(
                        axum::http::HeaderName::from_static(AITP_RECEIPT_HEADER),
                        value,
                    );
                }
                parts.headers.insert(
                    axum::http::HeaderName::from_static(AITP_VERSION_HEADER),
                    axum::http::HeaderValue::from_static(AITP_VERSION),
                );
            }
            if capped {
                parts.headers.insert(
                    "x-relay-billing-overage",
                    axum::http::HeaderValue::from_static("capped-to-reservation"),
                );
            }
        }
        None => {
            release_batch_poll_lease(&state.coordinator, &reservation).await;
            return batch_state_error("batch poll response could not be reconciled");
        }
    }
    Response::from_parts(parts, Body::from(bytes))
}

fn parse_batch_poll_capture(
    bytes: &[u8],
    prices: &PriceTable,
    model: &ModelId,
    expected_batch_id: uuid::Uuid,
) -> Option<BatchPollCapture> {
    let value: serde_json::Value = serde_json::from_slice(bytes).ok()?;
    match value.get("status")?.as_str()? {
        "pending" => Some(BatchPollCapture::Pending),
        "failed" => Some(BatchPollCapture::Failed),
        "completed" => {
            let batch_id = value
                .get("batch_id")?
                .as_str()
                .and_then(|raw| uuid::Uuid::parse_str(raw).ok())?;
            if batch_id != expected_batch_id {
                return None;
            }
            let inferences = value.get("inferences")?.as_array()?;
            if inferences.is_empty() {
                return None;
            }
            let mut prompt = 0_u32;
            let mut completion = 0_u32;
            for inference in inferences {
                let usage = inference.get("usage")?;
                prompt = prompt.checked_add(read_u32(usage, "input_tokens")?)?;
                completion = completion.checked_add(read_u32(usage, "output_tokens")?)?;
            }
            let total = prompt.checked_add(completion)?;
            Some(BatchPollCapture::Completed(Usage::new(
                prompt,
                completion,
                total,
                prices.cost(model, prompt, completion),
            )))
        }
        _ => None,
    }
}

async fn release_batch_poll_lease(
    coordinator: &Arc<DurableBillingCoordinator>,
    reservation: &Reservation,
) {
    if let Err(error) = coordinator.release_batch_poll(reservation).await {
        tracing::error!(error = %error, request_id = %reservation.job_id, "failed to release batch poll lease");
    }
}

/// RAII guard covering the reserve -> terminal window around the inference
/// handler. Armed at `reserve()`; on drop it releases the hold UNLESS it has
/// been `disarm`ed first. This makes the middleware panic-safe: if the handler
/// future unwinds, the guard drops and refunds the hold. Release is
/// terminal-idempotent, so a later explicit settle/release is unaffected.
struct HoldGuard {
    coordinator: Arc<DurableBillingCoordinator>,
    deferred_tasks: TaskTracker,
    reservation: Option<Reservation>,
}

impl HoldGuard {
    fn new(
        coordinator: Arc<DurableBillingCoordinator>,
        deferred_tasks: TaskTracker,
        reservation: Reservation,
    ) -> Self {
        Self {
            coordinator,
            deferred_tasks,
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
            // The durable reservation row is already committed. If this task is
            // torn down before the spawned release runs, startup recovery claims
            // the expired execution lease and releases the same hold.
            release_hold_detached(&self.deferred_tasks, &self.coordinator, &reservation);
        }
    }
}

/// Wraps a streaming response body with a [`StreamCarry`] Drop guard. As bytes
/// flow to the client, [`GuardedBody`] parses the terminal SSE `usage` frame;
/// on stream end the guard settles the exact actual cost (or releases if no
/// usage was ever seen).
#[expect(
    clippy::too_many_arguments,
    reason = "the stream guard must own each independent billing resource through body completion"
)]
fn attach_stream_carry(
    response: Response,
    coordinator: Arc<DurableBillingCoordinator>,
    evidence: Arc<dyn SettlementEvidenceStore>,
    prices: Arc<PriceTable>,
    deferred_tasks: TaskTracker,
    reservation: Reservation,
    requested_model: ModelId,
    route: InferenceRoute,
) -> Response {
    let (parts, body) = response.into_parts();
    let carry = StreamCarry {
        coordinator,
        evidence,
        deferred_tasks,
        reservation: Some(reservation),
        settled_usage: None,
        requested_model,
        route,
        failed: false,
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
    coordinator: Arc<DurableBillingCoordinator>,
    evidence: Arc<dyn SettlementEvidenceStore>,
    deferred_tasks: TaskTracker,
    reservation: Option<Reservation>,
    /// The terminal usage parsed off the SSE stream, if any. `Some` -> settle
    /// the exact actual on drop; `None` -> release the hold.
    settled_usage: Option<(Usage, ModelId)>,
    /// Request-derived model used only when a provider's terminal usage frame
    /// omits its otherwise stable model identifier.
    requested_model: ModelId,
    route: InferenceRoute,
    /// Sticky failure bit. Both body errors and TensorZero's ordinary SSE
    /// `{error: ...}` events set it; a later `[DONE]`/EOF can never clear it.
    failed: bool,
}

impl Drop for StreamCarry {
    fn drop(&mut self) {
        let Some(reservation) = self.reservation.take() else {
            return;
        };
        // The body was abandoned before its pollable finalizer took ownership.
        // Queue release now; if shutdown wins this race, the already-durable
        // execution row is released by recovery after its lease expires.
        release_hold_detached(&self.deferred_tasks, &self.coordinator, &reservation);
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
    finalizing: Option<futures::future::BoxFuture<'static, Result<(), BillingError>>>,
    withheld_data: Option<axum::body::Bytes>,
    pending_error: Option<axum::Error>,
    end_after_finalization: bool,
    heartbeat: tokio::time::Interval,
    renewing: Option<futures::future::BoxFuture<'static, Result<(), BillingError>>>,
    finished: bool,
}

impl GuardedBody {
    fn new(body: Body, prices: Arc<PriceTable>, carry: StreamCarry) -> Self {
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(300));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        Self {
            inner: http_body_util::BodyStream::new(body),
            prices,
            pending: String::new(),
            carry,
            finalizing: None,
            withheld_data: None,
            pending_error: None,
            end_after_finalization: false,
            heartbeat,
            renewing: None,
            finished: false,
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
            if event_has_error(&event) {
                self.carry.failed = true;
            }
            if let Some(found) =
                extract_usage_from_event(&event, &self.prices, &self.carry.requested_model)
            {
                self.carry.settled_usage = Some(found);
            }
        }
        // Guard against unbounded growth if a stream never emits a boundary.
        if self.pending.len() > MAX_RESPONSE_CAPTURE_BYTES {
            self.pending.clear();
        }
    }

    /// Flush any trailing event that lacked a final `\n\n` boundary.
    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let event = std::mem::take(&mut self.pending);
        if event_has_error(&event) {
            self.carry.failed = true;
        }
        if let Some(found) =
            extract_usage_from_event(&event, &self.prices, &self.carry.requested_model)
        {
            self.carry.settled_usage = Some(found);
        }
    }

    fn begin_finalization(&mut self, capture: Option<(Usage, ModelId)>) {
        if self.finalizing.is_some() || self.finished {
            return;
        }
        let Some(reservation) = self.carry.reservation.take() else {
            self.finished = true;
            return;
        };
        let coordinator = self.carry.coordinator.clone();
        let evidence = self.carry.evidence.clone();
        let terminal = capture;
        let route = self.carry.route;
        self.finalizing = Some(Box::pin(async move {
            finalize_stream(coordinator, evidence, reservation, terminal, route).await
        }));
    }

    fn begin_heartbeat(&mut self) {
        if self.renewing.is_some() || self.carry.reservation.is_none() {
            return;
        }
        let coordinator = self.carry.coordinator.clone();
        let Some(reservation) = self.carry.reservation.clone() else {
            return;
        };
        self.renewing = Some(Box::pin(async move {
            coordinator.renew_inference_lease(&reservation).await
        }));
    }
}

impl futures::Stream for GuardedBody {
    type Item = Result<axum::body::Bytes, axum::Error>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use futures::StreamExt;
        loop {
            if let Some(renewing) = self.renewing.as_mut()
                && let std::task::Poll::Ready(result) =
                    std::future::Future::poll(renewing.as_mut(), cx)
            {
                if let Err(error) = result {
                    tracing::error!(error = %error, "billing execution lease heartbeat failed");
                }
                self.renewing = None;
            }
            if std::pin::Pin::new(&mut self.heartbeat)
                .poll_tick(cx)
                .is_ready()
            {
                self.begin_heartbeat();
            }
            if let Some(finalizing) = self.finalizing.as_mut() {
                match std::future::Future::poll(finalizing.as_mut(), cx) {
                    std::task::Poll::Pending => return std::task::Poll::Pending,
                    std::task::Poll::Ready(result) => {
                        self.finalizing = None;
                        if let Err(error) = result {
                            self.finished = true;
                            return std::task::Poll::Ready(Some(Err(axum::Error::new(
                                std::io::Error::other(format!(
                                    "durable billing terminal staging failed: {error}"
                                )),
                            ))));
                        }
                        if let Some(error) = self.pending_error.take() {
                            self.finished = true;
                            return std::task::Poll::Ready(Some(Err(error)));
                        }
                        if let Some(data) = self.withheld_data.take() {
                            if self.end_after_finalization {
                                self.finished = true;
                            }
                            return std::task::Poll::Ready(Some(Ok(data)));
                        }
                        if self.end_after_finalization {
                            self.finished = true;
                            return std::task::Poll::Ready(None);
                        }
                        continue;
                    }
                }
            }
            if self.finished {
                return std::task::Poll::Ready(None);
            }

            match self.inner.poll_next_unpin(cx) {
                std::task::Poll::Ready(Some(Ok(frame))) => {
                    // Only forward data frames; trailers carry no bytes.
                    if let Ok(data) = frame.into_data() {
                        self.observe(&data);
                        if self.carry.reservation.is_some()
                            && (self.carry.failed
                                || self.carry.settled_usage.is_some()
                                || data.windows(b"[DONE]".len()).any(|w| w == b"[DONE]"))
                        {
                            if self.carry.failed {
                                self.withheld_data = Some(data);
                                self.begin_finalization(None);
                            } else if let Some(capture) = self.carry.settled_usage.take() {
                                self.withheld_data = Some(data);
                                self.begin_finalization(Some(capture));
                            } else {
                                // A nominally successful terminal marker without
                                // complete usage is not billable as success.
                                self.pending_error = Some(axum::Error::new(std::io::Error::other(
                                    "stream completed without trustworthy usage",
                                )));
                                self.end_after_finalization = true;
                                self.begin_finalization(None);
                            }
                            continue;
                        }
                        return std::task::Poll::Ready(Some(Ok(data)));
                    }
                    return std::task::Poll::Ready(Some(Ok(axum::body::Bytes::new())));
                }
                std::task::Poll::Ready(Some(Err(error))) => {
                    self.carry.failed = true;
                    self.pending_error = Some(axum::Error::new(error));
                    self.end_after_finalization = true;
                    self.begin_finalization(None);
                }
                std::task::Poll::Ready(None) => {
                    self.flush();
                    self.end_after_finalization = true;
                    let capture = if self.carry.failed {
                        None
                    } else {
                        self.carry.settled_usage.take()
                    };
                    self.begin_finalization(capture);
                }
                std::task::Poll::Pending => return std::task::Poll::Pending,
            }
        }
    }
}

/// Parse the LAST `data:` line in an SSE event that carries a `usage` object
/// into a billed [`Usage`] plus [`ModelId`]. `[DONE]` sentinels and usage-less
/// frames yield `None`.
fn extract_usage_from_event(
    event: &str,
    prices: &PriceTable,
    requested_model: &ModelId,
) -> Option<(Usage, ModelId)> {
    let mut last = None;
    for line in event.lines() {
        let Some(payload) = line.trim_start().strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            continue;
        }
        if let Some(found) =
            parse_usage_and_model_with_fallback(payload.as_bytes(), prices, Some(requested_model))
        {
            last = Some(found);
        }
    }
    last
}

/// TensorZero encodes provider failures as ordinary SSE JSON followed by
/// `[DONE]`; detect them at the application layer so clean EOF cannot turn a
/// failed generation into a maximum capture.
fn event_has_error(event: &str) -> bool {
    event.lines().any(|line| {
        let Some(payload) = line.trim_start().strip_prefix("data:") else {
            return false;
        };
        let payload = payload.trim();
        if payload.is_empty() || payload == "[DONE]" {
            return false;
        }
        serde_json::from_str::<serde_json::Value>(payload)
            .ok()
            .and_then(|value| value.get("error").cloned())
            .is_some_and(|error| !error.is_null())
    })
}

/// Fire-and-forget settle for a streamed job. A settle failure retains the hold:
/// releasing after the stream was delivered would make the successful call free.
async fn finalize_stream(
    coordinator: Arc<DurableBillingCoordinator>,
    evidence: Arc<dyn SettlementEvidenceStore>,
    reservation: Reservation,
    capture: Option<(Usage, ModelId)>,
    route: InferenceRoute,
) -> Result<(), BillingError> {
    match capture {
        Some((usage, model)) => {
            let (usage, _) = cap_usage_to_reservation(usage, &reservation);
            coordinator
                .stage_settle(&reservation, usage, model.clone(), route)
                .await?;
            match coordinator.settle(&reservation, usage, model, route).await {
                Ok(settlement) if settlement.receipt.is_some() => {
                    if let Err(err) = record_evidence(&evidence, &reservation, &settlement).await {
                        tracing::error!(error = %err, "streaming settlement evidence persistence failed");
                    }
                }
                Ok(_) => {}
                Err(err) => {
                    tracing::error!(error = %err, "streaming billing settle failed; retaining hold for recovery");
                }
            }
        }
        None => {
            coordinator.stage_release(&reservation).await?;
            release_hold(&coordinator, &reservation).await;
        }
    }
    Ok(())
}

async fn record_evidence(
    store: &Arc<dyn SettlementEvidenceStore>,
    reservation: &Reservation,
    settlement: &relay_billing::TerminalSettlement,
) -> Result<(), BillingError> {
    let receipt = settlement
        .receipt
        .clone()
        .ok_or_else(|| BillingError::Store("settlement proof has no Mint receipt".into()))?;
    store
        .record(&SettlementEvidence {
            request_id: reservation.job_id,
            principal: reservation.owner_principal.clone(),
            receipt: receipt.into(),
            attestation: settlement.attestation.clone(),
            verified_at_ms: settlement
                .attestation
                .as_ref()
                .map(|_| relay_billing::now_ms()),
            settled_at_ms: relay_billing::now_ms(),
        })
        .await
}

/// Match `GET .../billing/settlements/{uuid}` while allowing a configured base
/// path. `None` means this is not the evidence route; `Err` is a malformed id.
fn settlement_lookup_request(method: &Method, path: &str) -> Option<Result<JobId, ()>> {
    if method != Method::GET {
        return None;
    }
    let (_, raw) = path.rsplit_once("/billing/settlements/")?;
    if raw.is_empty() || raw.contains('/') {
        return Some(Err(()));
    }
    Some(JobId::parse(raw).ok_or(()))
}

fn settlement_lookup_error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "message": message,
                "type": "settlement_evidence_error",
                "code": "settlement_evidence_error",
            }
        })),
    )
        .into_response()
}

/// Cap an over-budget success to the amount the payer authorized. This prevents
/// Mint from rejecting `capture > hold` and, critically, prevents the old
/// fallback from releasing the successful request for free.
fn cap_usage_to_reservation(mut usage: Usage, reservation: &Reservation) -> (Usage, bool) {
    if usage.cost_micros <= reservation.held_amount_micros {
        return (usage, false);
    }
    tracing::error!(
        actual_micros = usage.cost_micros.get(),
        authorized_micros = reservation.held_amount_micros.get(),
        job_id = %reservation.job_id,
        "inference usage exceeded authorization; capping charge to held amount"
    );
    usage.cost_micros = reservation.held_amount_micros;
    (usage, true)
}

/// Release a durable hold. The ledger operation is idempotent and the outbox
/// keeps a failed terminal command replayable.
async fn release_hold(coordinator: &Arc<DurableBillingCoordinator>, reservation: &Reservation) {
    if let Err(error) = coordinator.release(reservation).await {
        tracing::error!(error = %error, request_id = %reservation.job_id, "durable billing release failed; queued command remains replayable");
    }
}

fn release_hold_detached(
    deferred_tasks: &TaskTracker,
    coordinator: &Arc<DurableBillingCoordinator>,
    reservation: &Reservation,
) {
    let coordinator = coordinator.clone();
    let reservation = reservation.clone();
    deferred_tasks.spawn(async move {
        release_hold(&coordinator, &reservation).await;
    });
}

/// True for the inference-shaped routes we bill: the native `/inference`, the
/// passport-conditioned generation surface (which reuses the native inference
/// handler), and the OpenAI-compatible chat/completions/responses/embeddings
/// endpoints.
fn is_billable_path(path: &str) -> bool {
    path.ends_with("/inference")
        || path.ends_with("/v1/passport/generate")
        || path.ends_with("/chat/completions")
        || path.ends_with("/completions")
        || path.ends_with("/responses")
        || path.ends_with("/embeddings")
}

fn is_batch_start(method: &Method, path: &str) -> bool {
    method == Method::POST && path.ends_with("/batch_inference")
}

/// Match the aggregate polling endpoint while allowing a configured base path.
fn whole_batch_poll(method: &Method, path: &str) -> Option<Result<uuid::Uuid, ()>> {
    if method != Method::GET {
        return None;
    }
    let (_, suffix) = path.rsplit_once("/batch_inference/")?;
    if suffix.is_empty() || suffix.contains('/') {
        return None;
    }
    Some(uuid::Uuid::parse_str(suffix).map_err(|_| ()))
}

fn is_individual_batch_poll(method: &Method, path: &str) -> bool {
    method == Method::GET
        && path
            .rsplit_once("/batch_inference/")
            .is_some_and(|(_, suffix)| suffix.contains("/inference/"))
}

fn batch_lookup_error(status: StatusCode, message: &str) -> Response {
    (
        status,
        Json(json!({
            "error": {
                "message": message,
                "type": "batch_billing_error",
                "code": "batch_billing_error",
            }
        })),
    )
        .into_response()
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
    parse_usage_and_model_with_fallback(bytes, prices, None)
}

fn parse_usage_and_model_with_fallback(
    bytes: &[u8],
    prices: &PriceTable,
    fallback_model: Option<&ModelId>,
) -> Option<(Usage, ModelId)> {
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
    let completion = read_u32(usage_obj, "completion_tokens")
        .or_else(|| read_u32(usage_obj, "output_tokens"))
        .or_else(|| is_embedding_response(&value).then_some(0));
    // Both dimensions are required for generative responses. OpenAI-compatible
    // embedding responses legitimately omit completion/output tokens; their
    // embedding data shape authorizes an explicit zero completion dimension.
    let prompt = prompt?;
    let completion = completion?;
    let computed_total = prompt.checked_add(completion)?;
    let total = match usage_obj.get("total_tokens") {
        Some(_) => {
            let supplied = read_u32(usage_obj, "total_tokens")?;
            if supplied != computed_total {
                return None;
            }
            supplied
        }
        None => computed_total,
    };

    // A function's explicit operator price is the customer-facing retail
    // contract independent of which TensorZero variant wins at runtime. Keep
    // that request identity for settlement so a response-level concrete model
    // cannot change the rate after authorization. Concrete-model requests,
    // however, settle against the authoritative served-model label.
    let model = match fallback_model {
        Some(requested) if requested.is_tensorzero_function() => requested.clone(),
        _ => billing_model_from_value(&value)
            .or_else(|| fallback_model.cloned())
            .unwrap_or_else(|| ModelId::namespace("unknown")),
    };

    // MODEL-AWARE cost: the exact actual usage priced at the served model's rate
    // (safe default for unknown models).
    let cost = prices.cost(&model, prompt, completion);
    let usage = Usage::new(prompt, completion, total, cost);

    Some((usage, model))
}

fn is_embedding_response(value: &serde_json::Value) -> bool {
    value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|items| {
            !items.is_empty() && items.iter().all(|item| item.get("embedding").is_some())
        })
}

fn read_u32(obj: &serde_json::Value, key: &str) -> Option<u32> {
    obj.get(key)
        .and_then(|v| v.as_u64())
        .map(|n| n.min(u32::MAX as u64) as u32)
}

/// Resolve both OpenAI-compatible and native TensorZero request/response model
/// vocabularies into Relay's stable billing key.
fn billing_model_from_value(value: &serde_json::Value) -> Option<ModelId> {
    if let Some(model) = value.get("model").and_then(|model| model.as_str()) {
        return Some(ModelId::from_tensorzero_selector(model));
    }
    if let Some(model_name) = value
        .get("model_name")
        .and_then(|model_name| model_name.as_str())
    {
        let selector = if model_name.starts_with("tensorzero::") {
            model_name.to_string()
        } else {
            format!("tensorzero::model_name::{model_name}")
        };
        return Some(ModelId::from_tensorzero_selector(&selector));
    }
    let function_name = value
        .get("function_name")
        .and_then(|function_name| function_name.as_str())?;
    let selector = if function_name.starts_with("tensorzero::") {
        function_name.to_string()
    } else {
        format!("tensorzero::function_name::{function_name}")
    };
    Some(ModelId::from_tensorzero_selector(&selector))
}

/// Buffer a billable request to derive its model and authorization ceilings,
/// then rebuild the same JSON for the downstream handler. Streaming chat calls
/// are forced to request a usage trailer so completed streams can settle actuals.
#[cfg(test)]
async fn buffer_request_pricing(request: Request) -> Result<(Request, RequestPricing), Response> {
    buffer_request_pricing_with_policy(request, EstimatePolicy::default()).await
}

async fn buffer_request_pricing_with_policy(
    request: Request,
    policy: EstimatePolicy,
) -> Result<(Request, RequestPricing), Response> {
    let unknown = ModelId::namespace("unknown");
    let path = request.uri().path().to_string();
    let batch_start = is_batch_start(request.method(), &path);
    let route = inference_route(&path);

    // Reject a body the router cannot accept before placing any authorization
    // hold. Pricing it as an unknown `u32::MAX`-token request would turn a
    // normal payload-size error into a misleading 402.
    if content_length_exceeds(request.headers(), MAX_REQUEST_BODY_BYTES) {
        return Err(request_body_too_large());
    }

    let (mut parts, body) = request.into_parts();
    let bytes = match axum::body::to_bytes(body, MAX_REQUEST_BODY_BYTES).await {
        Ok(bytes) => bytes,
        // This includes chunked/decompressed bodies whose actual size crosses
        // the limit despite lacking a trustworthy Content-Length.
        Err(_) => return Err(request_body_too_large()),
    };

    let mut value = serde_json::from_slice::<serde_json::Value>(&bytes).ok();
    let model = value
        .as_ref()
        .and_then(billing_model_from_value)
        .unwrap_or(unknown);
    let completion_token_ceiling = value.as_ref().and_then(|value| {
        if batch_start {
            batch_completion_token_ceiling(value, policy.hold_completion_tokens)
        } else {
            completion_token_ceiling(value)
        }
    });
    let mut streaming = false;
    if let Some(object) = value.as_mut().and_then(|v| v.as_object_mut()) {
        streaming = object.get("stream").and_then(|v| v.as_bool()) == Some(true);
        if streaming {
            let stream_options = object.entry("stream_options").or_insert_with(|| json!({}));
            if let Some(options) = stream_options.as_object_mut() {
                options.insert("include_usage".to_string(), json!(true));
            }
        }
    }
    let rebuilt = if streaming {
        value
            .and_then(|v| serde_json::to_vec(&v).ok())
            .unwrap_or_else(|| bytes.to_vec())
    } else {
        bytes.to_vec()
    };
    if parts
        .headers
        .contains_key(axum::http::header::CONTENT_LENGTH)
        && let Ok(length) = axum::http::HeaderValue::from_str(&rebuilt.len().to_string())
    {
        parts
            .headers
            .insert(axum::http::header::CONTENT_LENGTH, length);
    }
    let prompt_token_ceiling = bytes.len().min(u32::MAX as usize) as u32;

    Ok((
        Request::from_parts(parts, Body::from(rebuilt)),
        RequestPricing {
            model,
            route,
            prompt_token_ceiling,
            completion_token_ceiling,
        },
    ))
}

fn inference_route(path: &str) -> InferenceRoute {
    if path.ends_with("/openai/v1/chat/completions") {
        InferenceRoute::OpenAiChatCompletions
    } else if path.ends_with("/openai/v1/embeddings") {
        InferenceRoute::OpenAiEmbeddings
    } else if path.ends_with("/batch_inference") {
        InferenceRoute::BatchInference
    } else if path.ends_with("/inference") || path.ends_with("/v1/passport/generate") {
        // The passport surface delegates to the native inference handler, so its
        // cost events attribute to the same route class.
        InferenceRoute::NativeInference
    } else {
        InferenceRoute::Unknown
    }
}

fn batch_completion_token_ceiling(
    value: &serde_json::Value,
    default_per_inference: u32,
) -> Option<u32> {
    let input_count = value.get("inputs")?.as_array()?.len();
    let configured = value.pointer("/params/chat_completion/max_tokens");
    let configured = configured.and_then(serde_json::Value::as_array);
    let mut total = 0_u32;
    for index in 0..input_count {
        let ceiling = configured
            .and_then(|items| items.get(index))
            .and_then(serde_json::Value::as_u64)
            .map(|tokens| tokens.min(u32::MAX as u64) as u32)
            .unwrap_or(default_per_inference);
        total = total.saturating_add(ceiling);
    }
    Some(total)
}

fn completion_token_ceiling(value: &serde_json::Value) -> Option<u32> {
    read_u32(value, "max_completion_tokens")
        .or_else(|| read_u32(value, "max_output_tokens"))
        .or_else(|| read_u32(value, "max_tokens"))
        .or_else(|| {
            value
                .pointer("/params/chat_completion")
                .and_then(|params| read_u32(params, "max_tokens"))
        })
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

/// `413 Payload Too Large` emitted before a billing hold when request pricing
/// cannot faithfully buffer the gateway-sized payload.
fn request_body_too_large() -> Response {
    (
        StatusCode::PAYLOAD_TOO_LARGE,
        Json(json!({
            "error": {
                "message": "request body exceeds the 100 MiB gateway limit",
                "type": "request_too_large",
                "code": "request_too_large",
            }
        })),
    )
        .into_response()
}

/// `503 Service Unavailable` emitted before a hold or provider call when a
/// operator-configured TensorZero model/function has no explicit price.
fn billing_price_unavailable(model: &ModelId) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": {
                "message": format!("billing price unavailable for {model}"),
                "type": "billing_price_unavailable",
                "code": "billing_price_unavailable",
            }
        })),
    )
        .into_response()
}

fn already_settled_response(evidence: &SettlementEvidence) -> Response {
    let mut response = (
        StatusCode::CONFLICT,
        Json(json!({
            "error": {
                "message": "request already settled; use the attached evidence",
                "type": "request_already_settled",
                "code": "request_already_settled",
            },
            "data": evidence,
        })),
    )
        .into_response();

    if let (Some(balance_after_micros), Some(prev_hash)) = (
        evidence.receipt.balance_after_micros,
        evidence.receipt.prev_hash.clone(),
    ) {
        let receipt = relay_billing::aitp_client::Receipt {
            seq: evidence.receipt.sequence,
            op: evidence.receipt.operation.clone(),
            op_hash: evidence.receipt.operation_hash.clone(),
            amount_micros: relay_billing::aitp_client::AmountMicros::new(
                evidence.receipt.amount_micros,
            ),
            balance_after_micros: relay_billing::aitp_client::AmountMicros::new(
                balance_after_micros,
            ),
            prev_hash,
            hash: evidence.receipt.receipt_hash.clone(),
            sig: evidence.receipt.signature.clone(),
        };
        if let Ok(encoded) = relay_billing::aitp_client::codec::encode_receipt(&receipt)
            && let Ok(value) = axum::http::HeaderValue::from_str(&encoded)
        {
            response.headers_mut().insert(
                axum::http::HeaderName::from_static(AITP_RECEIPT_HEADER),
                value,
            );
        }
    }
    response.headers_mut().insert(
        axum::http::HeaderName::from_static(AITP_VERSION_HEADER),
        axum::http::HeaderValue::from_static(AITP_VERSION),
    );
    response
}

fn batch_state_error(message: &str) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "error": {
                "message": message,
                "type": "batch_billing_state_error",
                "code": "batch_billing_state_error",
            }
        })),
    )
        .into_response()
}

/// `502 Bad Gateway` returned when a successful upstream call could not be
/// committed to the billing ledger. The authorization is intentionally retained
/// for recovery; releasing it here would make the successful provider call free.
fn billing_settle_error() -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "error": {
                "message": "inference completed but billing settlement failed",
                "type": "billing_settlement_error",
                "code": "billing_settlement_error",
            }
        })),
    )
        .into_response()
}

/// The provider call and Mint capture succeeded, but the durable proof could
/// not be committed. Never present this as an ordinary successful response.
fn billing_evidence_error() -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "error": {
                "message": "inference settled but durable payment evidence was unavailable",
                "type": "billing_evidence_error",
                "code": "billing_evidence_error",
            }
        })),
    )
        .into_response()
}

fn aitp_authorization_error(message: &str) -> Response {
    (
        StatusCode::PAYMENT_REQUIRED,
        Json(json!({
            "error": {
                "message": message,
                "type": "invalid_aitp_authorization",
                "code": "invalid_aitp_authorization",
            }
        })),
    )
        .into_response()
}

/// `402 Payment Required` with an AITP-402 challenge (§5.1), returned before any
/// upstream call when the account cannot cover the model-aware hold. Carries the
/// `AITP-Version` and `AITP-Challenge` (base64url) headers alongside the existing
/// JSON error body, so both plain HTTP clients and AITP-aware agents are served.
fn aitp_payment_required(challenge: &AitpChallenge) -> Response {
    let encoded = challenge.encode().ok();

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
    if let Some(encoded) = encoded
        && let Ok(value) = axum::http::HeaderValue::from_str(&encoded)
    {
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
    use axum::routing::{get, post};
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
    const GPT4O_TENSORZERO: &str = "tensorzero::model_name::openai::gpt-4o";

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

    /// A price table: gpt-4o at $2.50/$10 per million tokens, safe default for
    /// everything else (the fake catalog).
    fn price_table() -> Arc<PriceTable> {
        Arc::new(
            PriceTable::new(default_model_price())
                .with_model(gpt4o(), ModelPrice::new(2_500_000, 10_000_000))
                .with_model(
                    ModelId::namespace("tensorzero::function_name::summarize"),
                    default_model_price(),
                )
                .with_model(
                    ModelId::namespace("tensorzero::function_name::write_copy"),
                    default_model_price(),
                ),
        )
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

    fn sse_response(events: &'static str) -> Response {
        Response::builder()
            .status(StatusCode::OK)
            .header(axum::http::header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(events))
            .expect("SSE response")
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
            // The billing middleware intercepts this authenticated lookup
            // before the placeholder handler runs.
            .route(
                "/v1/billing/settlements/{request_id}",
                get(|| async { StatusCode::NOT_FOUND }),
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

        // gpt-4o: 100*$2.50/1M + 50*$10/1M = 750 micro-USD exactly.
        assert_eq!(available(&ledger).await, 1_000_000 - 750);
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

        let expected_cost = default_model_price().cost(100, 50).get();
        assert_eq!(available(&ledger).await, 1_000_000 - expected_cost);
    }

    #[tokio::test]
    async fn unpriced_tensorzero_routes_fail_before_hold_or_provider_io() {
        let ledger = seeded_ledger(1_000_000).await;
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        for route in [
            "tensorzero::function_name::unpriced_route",
            "tensorzero::model_name::unpriced_route",
        ] {
            let handler_calls = calls.clone();
            let state = BillingState::new(
                ledger.clone(),
                EstimatePolicy::default(),
                Arc::new(PriceTable::new(default_model_price())),
                DEFAULT_AITP_PAYEE,
            );
            let ext = key_extension(ACCOUNT);
            let app = Router::new()
                .route(
                    "/v1/chat/completions",
                    post(move || {
                        let handler_calls = handler_calls.clone();
                        async move {
                            handler_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            ok_response(GPT4O_RAW, 100, 50)
                        }
                    }),
                )
                .layer(axum_middleware::from_fn_with_state(
                    state,
                    billing_middleware,
                ))
                .layer(axum_middleware::from_fn(
                    move |mut request: Request, next: Next| {
                        let ext = ext.clone();
                        async move {
                            request.extensions_mut().insert(ext);
                            next.run(request).await
                        }
                    },
                ));

            let response = app.oneshot(chat_request(route)).await.expect("response");
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert_eq!(available(&ledger).await, 1_000_000);
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
        let request_id = response.headers()[AITP_REQUEST_ID_HEADER].to_str().unwrap();

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
        // Model-aware hold: default budget (4000/4000) at $2.50/$10 per 1M.
        assert_eq!(challenge.amount_micros, 50_000);
        assert_eq!(
            challenge.resource,
            format!(
                "POST /v1/chat/completions#{}@{request_id}",
                gpt4o().as_str()
            )
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
        // at gpt-4o's $2.50/$10 per 1M = 50,000 micro-USD) from this account.
        let holds = fake.matching("POST", "/v1/holds");
        assert_eq!(holds.len(), 1, "exactly one hold created");
        let hold_body: serde_json::Value =
            serde_json::from_slice(&holds[0].body).expect("hold body json");
        assert_eq!(hold_body["from"], ACCOUNT);
        assert_eq!(hold_body["amount_micros"], 50_000);

        // CAPTURE at the exact actual (100*$2.50/1M + 50*$10/1M = 750) — mint
        // auto-releases the remainder, so the net debit is the actual.
        let caps = fake.matching("POST", "/v1/holds/hold_01FAKE/capture");
        assert_eq!(caps.len(), 1, "exactly one capture");
        let cap_body: serde_json::Value =
            serde_json::from_slice(&caps[0].body).expect("capture body json");
        assert_eq!(cap_body["amount_micros"], 750);
        assert_eq!(cap_body["to"], "acct_treasury");
    }

    #[tokio::test]
    async fn paid_batch_captures_once_on_mint_with_signed_receipt() {
        let fake = Arc::new(FakeMint::funded(100_000_000));
        let batch_id = uuid::Uuid::now_v7();
        let ext = key_extension(ACCOUNT);
        let app = Router::new()
            .route(
                "/batch_inference",
                post(move || async move {
                    Json(json!({
                        "batch_id": batch_id,
                        "inference_ids": [uuid::Uuid::now_v7()],
                        "episode_ids": [uuid::Uuid::now_v7()]
                    }))
                }),
            )
            .route(
                "/batch_inference/{batch_id}",
                get(move || async move {
                    Json(json!({
                        "status": "completed",
                        "batch_id": batch_id,
                        "inferences": [{
                            "inference_id": uuid::Uuid::now_v7(),
                            "usage": { "input_tokens": 100, "output_tokens": 50 }
                        }]
                    }))
                }),
            )
            .layer(axum_middleware::from_fn_with_state(
                mint_billing_state(mint_ledger(fake.clone())),
                billing_middleware,
            ))
            .layer(axum_middleware::from_fn(
                move |mut request: Request, next: Next| {
                    let ext = ext.clone();
                    async move {
                        request.extensions_mut().insert(ext);
                        next.run(request).await
                    }
                },
            ));
        let start = Request::builder()
            .method("POST")
            .uri("/batch_inference")
            .body(Body::from(
                json!({
                    "function_name": "summarize",
                    "inputs": [{"messages": []}]
                })
                .to_string(),
            ))
            .expect("batch start");
        assert_eq!(
            app.clone().oneshot(start).await.unwrap().status(),
            StatusCode::OK
        );

        let poll = Request::builder()
            .method("GET")
            .uri(format!("/batch_inference/{batch_id}"))
            .body(Body::empty())
            .expect("batch poll");
        let response = app.oneshot(poll).await.expect("completed batch");
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key(AITP_RECEIPT_HEADER));
        assert_eq!(fake.matching("POST", "/v1/holds").len(), 1);
        let captures = fake.matching("POST", "/v1/holds/hold_01FAKE/capture");
        assert_eq!(captures.len(), 1);
        let capture: serde_json::Value =
            serde_json::from_slice(&captures[0].body).expect("capture json");
        assert_eq!(capture["amount_micros"], 5_250);
    }

    #[tokio::test]
    async fn signed_receipt_is_retrievable_only_by_its_principal() {
        let fake = Arc::new(FakeMint::funded(100_000_000));
        let evidence = Arc::new(InMemorySettlementEvidenceStore::default());
        let evidence_store: Arc<dyn SettlementEvidenceStore> = evidence.clone();
        let state = mint_billing_state(mint_ledger(fake)).with_evidence_store(evidence_store);
        let app = app_with_account(state, key_extension(ACCOUNT), GPT4O_RAW);

        let response = app
            .clone()
            .oneshot(chat_request(GPT4O_RAW))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let request_id = response.headers()[AITP_REQUEST_ID_HEADER]
            .to_str()
            .expect("request id");

        let lookup = Request::builder()
            .method("GET")
            .uri(format!("/v1/billing/settlements/{request_id}"))
            .body(Body::empty())
            .unwrap();
        let found = app.clone().oneshot(lookup).await.unwrap();
        assert_eq!(found.status(), StatusCode::OK);
        let body = axum::body::to_bytes(found.into_body(), 32 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["data"]["principal"], ACCOUNT);
        assert_eq!(body["data"]["request_id"], request_id);
        assert_eq!(body["data"]["receipt"]["operation"], "hold.capture");
        assert!(
            body["data"]["receipt"]["signature"]
                .as_str()
                .is_some_and(|sig| !sig.is_empty())
        );

        let other = app_with_account(
            mint_billing_state(mint_ledger(Arc::new(FakeMint::funded(100_000_000))))
                .with_evidence_store(evidence),
            key_extension("acct_other"),
            GPT4O_RAW,
        );
        let lookup = Request::builder()
            .method("GET")
            .uri(format!("/v1/billing/settlements/{request_id}"))
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            other.oneshot(lookup).await.unwrap().status(),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn funded_requests_cannot_force_job_reuse_with_a_correlation_header() {
        let ledger = seeded_ledger(1_000_000).await;
        let app = app_with_account(billing_state(ledger), key_extension(ACCOUNT), GPT4O_RAW);
        let supplied = "11111111-1111-4111-8111-111111111111";
        let mut first = chat_request(GPT4O_RAW);
        first
            .headers_mut()
            .insert(AITP_REQUEST_ID_HEADER, supplied.parse().unwrap());
        let mut second = chat_request(GPT4O_RAW);
        second
            .headers_mut()
            .insert(AITP_REQUEST_ID_HEADER, supplied.parse().unwrap());

        let first = app.clone().oneshot(first).await.unwrap();
        let second = app.oneshot(second).await.unwrap();
        let first_id = first.headers()[AITP_REQUEST_ID_HEADER].to_str().unwrap();
        let second_id = second.headers()[AITP_REQUEST_ID_HEADER].to_str().unwrap();
        assert_ne!(first_id, supplied);
        assert_ne!(second_id, supplied);
        assert_ne!(first_id, second_id);
    }

    #[tokio::test]
    async fn dropped_execution_guard_refund_is_owned_by_shutdown_tracker() {
        let ledger = seeded_ledger(1_000_000).await;
        let coordinator = Arc::new(DurableBillingCoordinator::in_memory(ledger.clone()));
        let reservation = coordinator
            .reserve(
                &AccountId::new(ACCOUNT),
                Micros::new(250_000),
                JobId::default(),
            )
            .await
            .expect("reserve");
        coordinator
            .mark_inference_started(&reservation)
            .await
            .expect("execution owner");
        assert_eq!(available(&ledger).await, 750_000);

        let tracker = TaskTracker::new();
        drop(HoldGuard::new(coordinator, tracker.clone(), reservation));
        tracker.close();
        tracker.wait().await;

        assert_eq!(available(&ledger).await, 1_000_000);
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

    #[tokio::test]
    async fn valid_aitp_retry_is_verified_then_served_and_captured() {
        use relay_billing::aitp_client::{AmountMicros, PaymentAuthorization, Scheme};

        let fake = Arc::new(FakeMint::insufficient());
        let app = app_with_account(
            mint_billing_state(mint_ledger(fake.clone())),
            key_extension(ACCOUNT),
            GPT4O_RAW,
        );

        let first = app
            .clone()
            .oneshot(chat_request(GPT4O_RAW))
            .await
            .expect("402 response");
        assert_eq!(first.status(), StatusCode::PAYMENT_REQUIRED);
        let request_id = first
            .headers()
            .get(AITP_REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .expect("AITP request id")
            .to_string();
        let challenge = Challenge::decode(
            first
                .headers()
                .get(AITP_CHALLENGE_HEADER)
                .and_then(|value| value.to_str().ok())
                .expect("AITP challenge header"),
        )
        .expect("challenge decodes");
        let auth = PaymentAuthorization {
            payer: "acct_paid_agent".to_string(),
            payee: challenge.payee,
            amount_micros: AmountMicros(challenge.amount_micros),
            nonce: challenge.nonce,
            resource: challenge.resource,
            expiry: challenge.expiry as i64,
            scheme: Scheme::Hold,
            idempotency_key: "aitp:relay-retry".to_string(),
            pubkey_id: "payer-key".to_string(),
            op_hash: "signed-hash".to_string(),
            sig: "signed-intent".to_string(),
        };
        let encoded = relay_billing::aitp_client::codec::encode_authorization(&auth)
            .expect("authorization encodes");
        let mut retry = chat_request(GPT4O_RAW);
        retry.headers_mut().insert(
            AITP_AUTHORIZATION_HEADER,
            encoded.parse().expect("authorization header"),
        );
        retry.headers_mut().insert(
            AITP_REQUEST_ID_HEADER,
            request_id.parse().expect("request id header"),
        );

        let response = app
            .clone()
            .oneshot(retry)
            .await
            .expect("paid retry response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[AITP_REQUEST_ID_HEADER],
            request_id.as_str()
        );
        let receipt = response
            .headers()
            .get(AITP_RECEIPT_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| relay_billing::aitp_client::codec::decode_receipt(value).ok())
            .expect("signed settlement receipt");
        assert_eq!(receipt.op, "hold.capture");
        assert!(!receipt.op_hash.is_empty());
        assert!(!receipt.hash.is_empty());
        assert!(!receipt.sig.is_empty());
        assert_eq!(fake.matching("POST", "/v1/aitp/verify").len(), 1);
        assert_eq!(
            fake.matching("POST", "/v1/holds/hold_01FAKE/capture").len(),
            1
        );

        let mut replay = chat_request(GPT4O_RAW);
        replay.headers_mut().insert(
            AITP_AUTHORIZATION_HEADER,
            encoded.parse().expect("replay authorization header"),
        );
        replay.headers_mut().insert(
            AITP_REQUEST_ID_HEADER,
            request_id.parse().expect("replay request id header"),
        );
        let replay = app.clone().oneshot(replay).await.expect("replay response");
        assert_eq!(replay.status(), StatusCode::CONFLICT);
        assert!(replay.headers().contains_key(AITP_RECEIPT_HEADER));
        let replay_receipt = replay
            .headers()
            .get(AITP_RECEIPT_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| relay_billing::aitp_client::codec::decode_receipt(value).ok())
            .expect("replayed signed settlement receipt");
        assert_eq!(replay_receipt, receipt);
        let replay_body = axum::body::to_bytes(replay.into_body(), MAX_RESPONSE_CAPTURE_BYTES)
            .await
            .expect("replay body");
        let replay_json: serde_json::Value =
            serde_json::from_slice(&replay_body).expect("replay json");
        assert_eq!(
            replay_json.pointer("/error/code"),
            Some(&json!("request_already_settled"))
        );
        assert_eq!(
            fake.matching("POST", "/v1/holds/hold_01FAKE/capture").len(),
            1,
            "settled replay must not capture twice"
        );
        let lookup = Request::builder()
            .method("GET")
            .uri(format!("/v1/billing/settlements/{request_id}"))
            .body(Body::empty())
            .expect("lookup request");
        let lookup = app.oneshot(lookup).await.expect("lookup response");
        assert_eq!(lookup.status(), StatusCode::OK);
    }

    #[test]
    fn legacy_settlement_evidence_never_synthesizes_a_receipt_header() {
        let evidence = SettlementEvidence {
            request_id: JobId::default(),
            principal: AccountId::new(ACCOUNT),
            receipt: relay_billing::SettlementReceiptProof {
                sequence: 7,
                operation: "hold.capture".into(),
                operation_hash: "op".into(),
                amount_micros: 42,
                balance_after_micros: None,
                prev_hash: None,
                receipt_hash: "hash".into(),
                signature: "sig".into(),
            },
            attestation: None,
            verified_at_ms: None,
            settled_at_ms: 123,
        };

        let response = already_settled_response(&evidence);
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert!(!response.headers().contains_key(AITP_RECEIPT_HEADER));
        assert_eq!(
            response.headers().get(AITP_VERSION_HEADER),
            Some(&axum::http::HeaderValue::from_static(AITP_VERSION))
        );
    }

    #[tokio::test]
    async fn in_memory_http1_paid_journey_challenges_captures_and_recovers_replay() {
        use http_body_util::{BodyExt as _, Full};
        use hyper::client::conn::http1;
        use hyper::service::service_fn;
        use hyper_util::rt::TokioIo;
        use relay_billing::aitp_client::{AmountMicros, PaymentAuthorization, Scheme};

        let fake = Arc::new(FakeMint::insufficient());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let handler_calls = calls.clone();
        let state = mint_billing_state(mint_ledger(fake.clone()));
        let ext = key_extension(ACCOUNT);
        let app = Router::new()
            .route(
                "/v1/chat/completions",
                post(move || {
                    let handler_calls = handler_calls.clone();
                    async move {
                        handler_calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        ok_response(GPT4O_RAW, 100, 50)
                    }
                }),
            )
            .route(
                "/v1/billing/settlements/{request_id}",
                get(|| async { StatusCode::NOT_FOUND }),
            )
            .layer(axum_middleware::from_fn_with_state(
                state,
                billing_middleware,
            ))
            .layer(axum_middleware::from_fn(
                move |mut request: Request, next: Next| {
                    let ext = ext.clone();
                    async move {
                        let authenticated = request
                            .headers()
                            .get(axum::http::header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            == Some("Bearer relay-test-key");
                        if !authenticated {
                            return StatusCode::UNAUTHORIZED.into_response();
                        }
                        request.extensions_mut().insert(ext);
                        next.run(request).await
                    }
                },
            ));
        let (client_io, server_io) = tokio::io::duplex(256 * 1024);
        #[expect(
            clippy::disallowed_methods,
            reason = "the in-memory HTTP server task is explicitly awaited below"
        )]
        let server = tokio::spawn(async move {
            let service = service_fn(move |request: hyper::Request<hyper::body::Incoming>| {
                let app = app.clone();
                async move {
                    let response = app
                        .oneshot(request.map(Body::new))
                        .await
                        .expect("infallible Relay router");
                    Ok::<_, std::convert::Infallible>(response)
                }
            });
            hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(server_io), service)
                .await
                .expect("serve in-memory Relay HTTP/1 connection");
        });
        let (mut client, connection) =
            http1::handshake::<_, Full<axum::body::Bytes>>(TokioIo::new(client_io))
                .await
                .expect("open in-memory Relay HTTP/1 client");
        #[expect(
            clippy::disallowed_methods,
            reason = "the in-memory HTTP client driver is explicitly awaited below"
        )]
        let client_connection = tokio::spawn(async move {
            connection.await.expect("drive Relay HTTP/1 client");
        });
        let body = json!({ "model": GPT4O_RAW, "messages": [] }).to_string();

        let unauthenticated_request = hyper::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .body(Full::new(axum::body::Bytes::from(body.clone())))
            .expect("unauthenticated request");
        let unauthenticated = client
            .send_request(unauthenticated_request)
            .await
            .expect("unauthenticated response");
        assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);
        unauthenticated
            .into_body()
            .collect()
            .await
            .expect("drain unauthenticated response");

        let first_request = hyper::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .header(axum::http::header::AUTHORIZATION, "Bearer relay-test-key")
            .body(Full::new(axum::body::Bytes::from(body.clone())))
            .expect("initial paid request");
        let first = client
            .send_request(first_request)
            .await
            .expect("initial paid request");
        assert_eq!(first.status(), StatusCode::PAYMENT_REQUIRED);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        let request_id = first
            .headers()
            .get(AITP_REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .expect("request id")
            .to_string();
        let challenge = Challenge::decode(
            first
                .headers()
                .get(AITP_CHALLENGE_HEADER)
                .and_then(|value| value.to_str().ok())
                .expect("challenge header"),
        )
        .expect("challenge");
        first
            .into_body()
            .collect()
            .await
            .expect("drain challenge response");
        let authorization = PaymentAuthorization {
            payer: "acct_paid_agent".into(),
            payee: challenge.payee,
            amount_micros: AmountMicros::new(challenge.amount_micros),
            nonce: challenge.nonce,
            resource: challenge.resource,
            expiry: challenge.expiry as i64,
            scheme: Scheme::Hold,
            idempotency_key: "aitp:socket-acceptance".into(),
            pubkey_id: "payer-key".into(),
            op_hash: "signed-hash".into(),
            sig: "signed-intent".into(),
        };
        let encoded = relay_billing::aitp_client::codec::encode_authorization(&authorization)
            .expect("authorization header");

        let paid_request = hyper::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .header(axum::http::header::AUTHORIZATION, "Bearer relay-test-key")
            .header(AITP_REQUEST_ID_HEADER, &request_id)
            .header(AITP_AUTHORIZATION_HEADER, &encoded)
            .body(Full::new(axum::body::Bytes::from(body.clone())))
            .expect("paid retry request");
        let paid = client.send_request(paid_request).await.expect("paid retry");
        assert_eq!(paid.status(), StatusCode::OK);
        assert!(paid.headers().contains_key(AITP_RECEIPT_HEADER));
        paid.into_body()
            .collect()
            .await
            .expect("drain paid response");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            fake.matching("POST", "/v1/holds/hold_01FAKE/capture").len(),
            1
        );

        let replay_request = hyper::Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(axum::http::header::CONTENT_TYPE, "application/json")
            .header(axum::http::header::AUTHORIZATION, "Bearer relay-test-key")
            .header(AITP_REQUEST_ID_HEADER, &request_id)
            .header(AITP_AUTHORIZATION_HEADER, &encoded)
            .body(Full::new(axum::body::Bytes::from(body)))
            .expect("settled replay request");
        let replay = client
            .send_request(replay_request)
            .await
            .expect("settled replay");
        assert_eq!(replay.status(), StatusCode::CONFLICT);
        assert!(replay.headers().contains_key(AITP_RECEIPT_HEADER));
        let replay_body = replay
            .into_body()
            .collect()
            .await
            .expect("replay evidence body")
            .to_bytes();
        let replay_json: serde_json::Value =
            serde_json::from_slice(&replay_body).expect("replay evidence json");
        assert_eq!(
            replay_json
                .pointer("/data/request_id")
                .and_then(|value| value.as_str()),
            Some(request_id.as_str())
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            fake.matching("POST", "/v1/holds/hold_01FAKE/capture").len(),
            1
        );

        let evidence_request = hyper::Request::builder()
            .method("GET")
            .uri(format!("/v1/billing/settlements/{request_id}"))
            .header(axum::http::header::AUTHORIZATION, "Bearer relay-test-key")
            .body(Full::new(axum::body::Bytes::new()))
            .expect("evidence lookup request");
        let evidence = client
            .send_request(evidence_request)
            .await
            .expect("evidence lookup");
        assert_eq!(evidence.status(), StatusCode::OK);
        evidence
            .into_body()
            .collect()
            .await
            .expect("drain evidence response");
        drop(client);
        client_connection.await.expect("HTTP client task");
        server.await.expect("HTTP server task");
    }

    #[tokio::test]
    async fn aitp_retry_cannot_be_stolen_by_another_authenticated_principal() {
        use relay_billing::aitp_client::{AmountMicros, PaymentAuthorization, Scheme};

        let fake = Arc::new(FakeMint::insufficient());
        let state = mint_billing_state(mint_ledger(fake.clone()));
        let owner = app_with_account(state.clone(), key_extension(ACCOUNT), GPT4O_RAW);
        let attacker = app_with_account(state, key_extension("acct_attacker"), GPT4O_RAW);

        let first = owner
            .oneshot(chat_request(GPT4O_RAW))
            .await
            .expect("owner challenge response");
        let request_id = first.headers()[AITP_REQUEST_ID_HEADER]
            .to_str()
            .expect("request id")
            .to_string();
        let challenge = Challenge::decode(
            first.headers()[AITP_CHALLENGE_HEADER]
                .to_str()
                .expect("challenge header"),
        )
        .expect("challenge decodes");
        let authorization = PaymentAuthorization {
            payer: "acct_paid_agent".into(),
            payee: challenge.payee,
            amount_micros: AmountMicros(challenge.amount_micros),
            nonce: challenge.nonce,
            resource: challenge.resource,
            expiry: challenge.expiry as i64,
            scheme: Scheme::Hold,
            idempotency_key: "aitp:stolen-retry".into(),
            pubkey_id: "payer-key".into(),
            op_hash: "signed-hash".into(),
            sig: "signed-intent".into(),
        };
        let mut retry = chat_request(GPT4O_RAW);
        retry.headers_mut().insert(
            AITP_AUTHORIZATION_HEADER,
            relay_billing::aitp_client::codec::encode_authorization(&authorization)
                .expect("authorization encodes")
                .parse()
                .expect("authorization header"),
        );
        retry.headers_mut().insert(
            AITP_REQUEST_ID_HEADER,
            request_id.parse().expect("request id header"),
        );

        let response = attacker.oneshot(retry).await.expect("attacker response");
        assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
        assert!(
            !response.headers().contains_key(AITP_CHALLENGE_HEADER),
            "an ownership failure must not mint a replacement challenge"
        );
        assert!(
            fake.matching("POST", "/v1/aitp/verify").is_empty(),
            "ownership rejection must happen before Mint verification"
        );
        assert!(
            fake.matching("POST", "/v1/holds/hold_01FAKE/capture")
                .is_empty(),
            "stolen challenge must never capture"
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
    async fn paid_batch_holds_across_pending_then_settles_aggregate_usage() {
        let ledger = seeded_ledger(1_000_000).await;
        let batch_id = uuid::Uuid::now_v7();
        let polls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let poll_counter = polls.clone();
        let completed_batch_id = batch_id;
        let ext = key_extension(ACCOUNT);
        let app = Router::new()
            .route(
                "/batch_inference",
                post(move || async move {
                    Json(json!({
                        "batch_id": batch_id,
                        "inference_ids": [uuid::Uuid::now_v7(), uuid::Uuid::now_v7()],
                        "episode_ids": [uuid::Uuid::now_v7(), uuid::Uuid::now_v7()]
                    }))
                }),
            )
            .route(
                "/batch_inference/{batch_id}",
                get(move || {
                    let poll = poll_counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    async move {
                        if poll == 0 {
                            Json(json!({ "status": "pending" }))
                        } else {
                            Json(json!({
                                "status": "completed",
                                "batch_id": completed_batch_id,
                                "inferences": [
                                    {
                                        "inference_id": uuid::Uuid::now_v7(),
                                        "usage": { "input_tokens": 100, "output_tokens": 50 }
                                    },
                                    {
                                        "inference_id": uuid::Uuid::now_v7(),
                                        "usage": { "input_tokens": 200, "output_tokens": 25 }
                                    }
                                ]
                            }))
                        }
                    }
                }),
            )
            .layer(axum_middleware::from_fn_with_state(
                billing_state(ledger.clone()),
                billing_middleware,
            ))
            .layer(axum_middleware::from_fn(
                move |mut request: Request, next: Next| {
                    let ext = ext.clone();
                    async move {
                        request.extensions_mut().insert(ext);
                        next.run(request).await
                    }
                },
            ));
        let start = Request::builder()
            .method("POST")
            .uri("/batch_inference")
            .body(Body::from(
                json!({
                    "function_name": "summarize",
                    "inputs": [{"messages": []}, {"messages": []}]
                })
                .to_string(),
            ))
            .expect("batch request");

        let started = app.clone().oneshot(start).await.expect("batch start");
        assert_eq!(started.status(), StatusCode::OK);
        let held_balance = available(&ledger).await;
        assert!(held_balance < 1_000_000, "batch start must place a hold");

        let poll_request = || {
            Request::builder()
                .method("GET")
                .uri(format!("/batch_inference/{batch_id}"))
                .body(Body::empty())
                .expect("poll request")
        };
        let pending = app
            .clone()
            .oneshot(poll_request())
            .await
            .expect("pending response");
        assert_eq!(pending.status(), StatusCode::OK);
        assert_eq!(available(&ledger).await, held_balance);

        let completed = app
            .oneshot(poll_request())
            .await
            .expect("completed response");
        assert_eq!(completed.status(), StatusCode::OK);
        assert!(completed.headers().contains_key(AITP_REQUEST_ID_HEADER));
        let model = ModelId::namespace("tensorzero::function_name::summarize");
        let exact = price_table().cost(&model, 300, 75).get();
        assert_eq!(available(&ledger).await, 1_000_000 - exact);
    }

    #[tokio::test]
    async fn failed_paid_batch_releases_its_entire_hold() {
        let ledger = seeded_ledger(1_000_000).await;
        let batch_id = uuid::Uuid::now_v7();
        let ext = key_extension(ACCOUNT);
        let app = Router::new()
            .route(
                "/batch_inference",
                post(move || async move {
                    Json(json!({
                        "batch_id": batch_id,
                        "inference_ids": [uuid::Uuid::now_v7()],
                        "episode_ids": [uuid::Uuid::now_v7()]
                    }))
                }),
            )
            .route(
                "/batch_inference/{batch_id}",
                get(|| async { Json(json!({ "status": "failed" })) }),
            )
            .layer(axum_middleware::from_fn_with_state(
                billing_state(ledger.clone()),
                billing_middleware,
            ))
            .layer(axum_middleware::from_fn(
                move |mut request: Request, next: Next| {
                    let ext = ext.clone();
                    async move {
                        request.extensions_mut().insert(ext);
                        next.run(request).await
                    }
                },
            ));
        let start = Request::builder()
            .method("POST")
            .uri("/batch_inference")
            .body(Body::from(
                json!({
                    "function_name": "summarize",
                    "inputs": [{"messages": []}]
                })
                .to_string(),
            ))
            .expect("batch start");
        assert_eq!(
            app.clone().oneshot(start).await.unwrap().status(),
            StatusCode::OK
        );
        assert!(available(&ledger).await < 1_000_000);

        let poll = Request::builder()
            .method("GET")
            .uri(format!("/batch_inference/{batch_id}"))
            .body(Body::empty())
            .expect("batch poll");
        assert_eq!(app.oneshot(poll).await.unwrap().status(), StatusCode::OK);
        assert_eq!(available(&ledger).await, 1_000_000);
    }

    #[tokio::test]
    async fn billing_disabled_by_default_env() {
        // The safety gate stays OFF unless explicitly opted in.
        assert!(!billing_enabled() || std::env::var("RELAY_BILLING_ENABLED").is_ok());
    }

    // ---- unit: request/response model extraction -------------------------

    #[tokio::test]
    async fn buffer_request_pricing_extracts_model_and_limits() {
        let request = chat_request(GPT4O_RAW);
        let (rebuilt, pricing) = buffer_request_pricing(request)
            .await
            .expect("request pricing");
        assert_eq!(pricing.model, gpt4o());
        assert!(pricing.prompt_token_ceiling > 0);
        assert_eq!(pricing.completion_token_ceiling, None);
        // Body preserved for the downstream handler.
        let bytes = axum::body::to_bytes(rebuilt.into_body(), MAX_RESPONSE_CAPTURE_BYTES)
            .await
            .expect("body");
        assert!(!bytes.is_empty());
    }

    #[tokio::test]
    async fn multimodal_sized_request_above_response_cap_remains_billable() {
        let large_input = "x".repeat(MAX_RESPONSE_CAPTURE_BYTES);
        let body = format!(r#"{{"model":"{GPT4O_RAW}","input":"{large_input}"}}"#);
        assert!(body.len() > MAX_RESPONSE_CAPTURE_BYTES);
        assert!(body.len() < MAX_REQUEST_BODY_BYTES);
        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header(axum::http::header::CONTENT_LENGTH, body.len())
            .body(Body::from(body.clone()))
            .expect("large request");

        let (rebuilt, pricing) = buffer_request_pricing(request)
            .await
            .expect("gateway-sized request remains priceable");

        assert_eq!(pricing.model, gpt4o());
        assert_eq!(pricing.prompt_token_ceiling, body.len() as u32);
        let bytes = axum::body::to_bytes(rebuilt.into_body(), MAX_REQUEST_BODY_BYTES)
            .await
            .expect("preserved request");
        assert_eq!(bytes.as_ref(), body.as_bytes());
    }

    #[tokio::test]
    async fn streaming_request_rebuild_updates_content_length() {
        let body = format!("{{  \"model\": \"{GPT4O_RAW}\", \"messages\": [], \"stream\": true }}");
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header(axum::http::header::CONTENT_LENGTH, body.len())
            .body(Body::from(body))
            .expect("streaming request");

        let (rebuilt, _) = buffer_request_pricing(request)
            .await
            .expect("request pricing");
        let declared_length = rebuilt
            .headers()
            .get(axum::http::header::CONTENT_LENGTH)
            .expect("content length")
            .to_str()
            .expect("ascii length")
            .parse::<usize>()
            .expect("numeric length");
        let bytes = axum::body::to_bytes(rebuilt.into_body(), MAX_REQUEST_BODY_BYTES)
            .await
            .expect("rebuilt body");

        assert_eq!(declared_length, bytes.len());
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json body");
        assert_eq!(
            value.pointer("/stream_options/include_usage"),
            Some(&json!(true))
        );
    }

    #[tokio::test]
    async fn known_oversized_request_is_rejected_before_pricing() {
        let request = Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header(
                axum::http::header::CONTENT_LENGTH,
                MAX_REQUEST_BODY_BYTES + 1,
            )
            .body(Body::empty())
            .expect("oversized request");

        let response = buffer_request_pricing(request)
            .await
            .expect_err("oversized body must fail closed");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn tensorzero_model_selector_uses_catalog_for_hold_estimate() {
        let request = chat_request(GPT4O_TENSORZERO);
        let (_rebuilt, pricing) = buffer_request_pricing(request)
            .await
            .expect("request pricing");
        assert_eq!(pricing.model, gpt4o());
        assert_eq!(
            EstimatePolicy::default().hold_estimate(&price_table(), &pricing),
            price_table().cost(&gpt4o(), pricing.prompt_token_ceiling.max(4_000), 4_000)
        );
    }

    #[tokio::test]
    async fn native_inference_model_and_function_names_get_stable_billing_keys() {
        let model_request = Request::builder()
            .method("POST")
            .uri("/inference")
            .body(Body::from(
                json!({ "model_name": "openai::gpt-4o", "input": {} }).to_string(),
            ))
            .expect("native model request");
        let (_rebuilt, model_pricing) = buffer_request_pricing(model_request)
            .await
            .expect("model request pricing");
        assert_eq!(model_pricing.model, gpt4o());

        let configured_model_request = Request::builder()
            .method("POST")
            .uri("/inference")
            .body(Body::from(
                json!({ "model_name": "production-chat", "input": {} }).to_string(),
            ))
            .expect("configured model request");
        let (_rebuilt, configured_model_pricing) = buffer_request_pricing(configured_model_request)
            .await
            .expect("configured model request pricing");
        assert_eq!(
            configured_model_pricing.model,
            ModelId::namespace("tensorzero::model_name::production-chat")
        );

        let function_request = Request::builder()
            .method("POST")
            .uri("/inference")
            .body(Body::from(
                json!({ "function_name": "write_copy", "input": {} }).to_string(),
            ))
            .expect("native function request");
        let (_rebuilt, function_pricing) = buffer_request_pricing(function_request)
            .await
            .expect("function request pricing");
        assert_eq!(
            function_pricing.model,
            ModelId::namespace("tensorzero::function_name::write_copy")
        );
    }

    #[tokio::test]
    async fn native_nested_max_tokens_sizes_the_authorization_hold() {
        let request = Request::builder()
            .method("POST")
            .uri("/inference")
            .body(Body::from(
                json!({
                    "function_name": "write_copy",
                    "input": { "messages": [] },
                    "params": { "chat_completion": { "max_tokens": 50_000 } }
                })
                .to_string(),
            ))
            .expect("native inference request");
        let (_rebuilt, pricing) = buffer_request_pricing(request)
            .await
            .expect("request pricing");
        assert_eq!(pricing.completion_token_ceiling, Some(50_000));
        assert_eq!(
            EstimatePolicy::default().hold_estimate(&price_table(), &pricing),
            price_table().cost(
                &ModelId::namespace("tensorzero::function_name::write_copy"),
                pricing.prompt_token_ceiling.max(4_000),
                50_000,
            )
        );
    }

    #[tokio::test]
    async fn batch_hold_sums_explicit_and_default_output_ceilings() {
        let request = Request::builder()
            .method("POST")
            .uri("/batch_inference")
            .body(Body::from(
                json!({
                    "function_name": "summarize",
                    "inputs": [{}, {}, {}],
                    "params": {
                        "chat_completion": { "max_tokens": [100, null, 200] }
                    }
                })
                .to_string(),
            ))
            .expect("batch request");
        let (_rebuilt, pricing) = buffer_request_pricing(request)
            .await
            .expect("batch pricing");
        assert_eq!(
            pricing.model,
            ModelId::namespace("tensorzero::function_name::summarize")
        );
        assert_eq!(pricing.completion_token_ceiling, Some(4_300));
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
        assert_eq!(usage.cost_micros, Micros::new(750));
    }

    #[test]
    fn embedding_usage_accepts_zero_output_only_for_embedding_payloads() {
        let prices = price_table();
        let embedding = json!({
            "model": GPT4O_RAW,
            "data": [{ "object": "embedding", "index": 0, "embedding": [0.1, 0.2] }],
            "usage": { "prompt_tokens": 100, "total_tokens": 100 }
        });
        let (usage, model) = parse_usage_and_model(embedding.to_string().as_bytes(), &prices)
            .expect("embedding usage");
        assert_eq!(model, gpt4o());
        assert_eq!(usage.prompt_tokens, 100);
        assert_eq!(usage.completion_tokens, 0);
        assert_eq!(usage.cost_micros, Micros::new(250));

        let ambiguous = json!({
            "model": GPT4O_RAW,
            "usage": { "prompt_tokens": 100, "total_tokens": 100 }
        });
        assert!(parse_usage_and_model(ambiguous.to_string().as_bytes(), &prices).is_none());
    }

    #[tokio::test]
    async fn authenticated_embedding_request_settles_prompt_usage() {
        let ledger = seeded_ledger(1_000_000).await;
        let ext = key_extension(ACCOUNT);
        let app = Router::new()
            .route(
                "/v1/embeddings",
                post(|| async {
                    (
                        StatusCode::OK,
                        Json(json!({
                            "model": GPT4O_RAW,
                            "data": [{ "object": "embedding", "index": 0, "embedding": [0.1] }],
                            "usage": { "prompt_tokens": 100, "total_tokens": 100 }
                        })),
                    )
                }),
            )
            .layer(axum_middleware::from_fn_with_state(
                billing_state(ledger.clone()),
                billing_middleware,
            ))
            .layer(axum_middleware::from_fn(
                move |mut request: Request, next: Next| {
                    let ext = ext.clone();
                    async move {
                        request.extensions_mut().insert(ext);
                        next.run(request).await
                    }
                },
            ));
        let request = Request::builder()
            .method("POST")
            .uri("/v1/embeddings")
            .header("content-type", "application/json")
            .body(Body::from(
                json!({ "model": GPT4O_RAW, "input": "hello" }).to_string(),
            ))
            .expect("embedding request");

        let response = app.oneshot(request).await.expect("embedding response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(available(&ledger).await, 999_750);
    }

    #[test]
    fn tensorzero_response_selector_settles_at_authoritative_model_rate() {
        let prices = price_table();
        let body = json!({
            "model": GPT4O_TENSORZERO,
            "usage": { "prompt_tokens": 100, "completion_tokens": 50 }
        })
        .to_string();
        let (usage, model) = parse_usage_and_model(body.as_bytes(), &prices).expect("usage parsed");
        assert_eq!(model, gpt4o());
        assert_eq!(usage.cost_micros, Micros::new(750));
    }

    #[test]
    fn tensorzero_function_variant_uses_stable_operator_configured_price() {
        let function = ModelId::namespace("tensorzero::function_name::write_copy");
        let prices = Arc::new(
            PriceTable::new(default_model_price())
                .with_model(function.clone(), ModelPrice::new(20_000_000, 40_000_000)),
        );
        let selector = "tensorzero::function_name::write_copy::variant_name::quality_route";
        let body = json!({
            "model": selector,
            "usage": { "prompt_tokens": 100, "completion_tokens": 50 }
        })
        .to_string();
        let (usage, model) = parse_usage_and_model(body.as_bytes(), &prices).expect("usage parsed");
        assert_eq!(model, function);
        assert_eq!(usage.cost_micros, Micros::new(4_000));
    }

    #[test]
    fn native_function_usage_uses_request_level_function_identity() {
        let function = ModelId::namespace("tensorzero::function_name::write_copy");
        let prices = Arc::new(
            PriceTable::new(default_model_price())
                .with_model(function.clone(), ModelPrice::new(20_000_000, 40_000_000)),
        );
        let body = json!({
            "function_name": "write_copy",
            "usage": { "input_tokens": 100, "output_tokens": 50 }
        })
        .to_string();
        let (usage, model) = parse_usage_and_model(body.as_bytes(), &prices).expect("usage parsed");
        assert_eq!(model, function);
        assert_eq!(usage.cost_micros, Micros::new(4_000));
    }

    #[test]
    fn function_settlement_cannot_be_repriced_by_response_model() {
        let function = ModelId::namespace("tensorzero::function_name::write_copy");
        let prices = Arc::new(
            PriceTable::new(default_model_price())
                .with_model(function.clone(), ModelPrice::new(20_000_000, 40_000_000))
                .with_model(gpt4o(), ModelPrice::new(2_500_000, 10_000_000)),
        );
        let body = json!({
            "model": GPT4O_TENSORZERO,
            "usage": { "prompt_tokens": 100, "completion_tokens": 50 }
        })
        .to_string();

        let (usage, settled_model) =
            parse_usage_and_model_with_fallback(body.as_bytes(), &prices, Some(&function))
                .expect("usage parsed");

        assert_eq!(settled_model, function);
        assert_eq!(usage.cost_micros, Micros::new(4_000));
    }

    #[test]
    fn usage_requires_both_token_dimensions_and_consistent_total() {
        let prices = price_table();
        for usage in [
            json!({}),
            json!({ "prompt_tokens": 10 }),
            json!({ "completion_tokens": 5 }),
            json!({ "prompt_tokens": "10", "completion_tokens": 5 }),
            json!({ "prompt_tokens": 10, "completion_tokens": "5" }),
            json!({ "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 99 }),
        ] {
            let body = json!({ "model": GPT4O_RAW, "usage": usage }).to_string();
            assert!(
                parse_usage_and_model(body.as_bytes(), &prices).is_none(),
                "incomplete or inconsistent usage must fail closed: {body}"
            );
        }

        let explicit_zero = json!({
            "model": GPT4O_RAW,
            "usage": {
                "prompt_tokens": 0,
                "completion_tokens": 0,
                "total_tokens": 0
            }
        })
        .to_string();
        assert!(parse_usage_and_model(explicit_zero.as_bytes(), &prices).is_some());
    }

    #[test]
    fn tensorzero_error_sse_is_sticky_even_with_done_after_it() {
        let native = "data: {\"error\":{\"message\":\"provider failed\"}}\n\n";
        let openai = "data: {\"error\":{\"type\":\"server_error\"}}\n\n";
        assert!(event_has_error(native));
        assert!(event_has_error(openai));
        assert!(!event_has_error("data: [DONE]\n\n"));
        assert!(!event_has_error(
            "data: {\"choices\":[],\"usage\":null}\n\n"
        ));
    }

    #[tokio::test]
    async fn streaming_terminal_usage_is_settled_before_done_is_delivered() {
        let ledger = seeded_ledger(1_000_000).await;
        let state = billing_state(ledger.clone());
        let ext = key_extension(ACCOUNT);
        let app = Router::new()
            .route(
                "/v1/chat/completions",
                post(|| async {
                    sse_response(
                        "data: {\"usage\":{\"prompt_tokens\":100,\"completion_tokens\":50,\"total_tokens\":150}}\n\ndata: [DONE]\n\n",
                    )
                }),
            )
            .layer(axum_middleware::from_fn_with_state(
                state,
                billing_middleware,
            ))
            .layer(axum_middleware::from_fn(
                move |mut request: Request, next: Next| {
                    let ext = ext.clone();
                    async move {
                        request.extensions_mut().insert(ext);
                        next.run(request).await
                    }
                },
            ));

        let response = app.oneshot(chat_request(GPT4O_RAW)).await.unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("durably settled stream");
        assert!(String::from_utf8_lossy(&bytes).contains("[DONE]"));
        assert_eq!(available(&ledger).await, 1_000_000 - 750);
    }

    #[tokio::test]
    async fn streaming_done_without_complete_usage_fails_and_releases() {
        let ledger = seeded_ledger(1_000_000).await;
        let state = billing_state(ledger.clone());
        let ext = key_extension(ACCOUNT);
        let app = Router::new()
            .route(
                "/v1/chat/completions",
                post(|| async {
                    sse_response(
                        "data: {\"model\":\"openai/gpt-4o\",\"usage\":{\"prompt_tokens\":100}}\n\ndata: [DONE]\n\n",
                    )
                }),
            )
            .layer(axum_middleware::from_fn_with_state(
                state,
                billing_middleware,
            ))
            .layer(axum_middleware::from_fn(
                move |mut request: Request, next: Next| {
                    let ext = ext.clone();
                    async move {
                        request.extensions_mut().insert(ext);
                        next.run(request).await
                    }
                },
            ));

        let response = app.oneshot(chat_request(GPT4O_RAW)).await.unwrap();
        assert!(
            axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .is_err(),
            "unmeterable terminal success must not expose [DONE]"
        );
        assert_eq!(available(&ledger).await, 1_000_000);
    }

    #[tokio::test]
    async fn successful_body_without_usage_is_not_delivered_for_free() {
        let ledger = seeded_ledger(1_000_000).await;
        let state = billing_state(ledger.clone());
        let ext = key_extension(ACCOUNT);
        let app = Router::new()
            .route(
                "/v1/chat/completions",
                post(|| async {
                    (
                        StatusCode::OK,
                        Json(json!({ "model": GPT4O_RAW, "choices": [] })),
                    )
                        .into_response()
                }),
            )
            .layer(axum_middleware::from_fn_with_state(
                state,
                billing_middleware,
            ))
            .layer(axum_middleware::from_fn(
                move |mut request: Request, next: Next| {
                    let ext = ext.clone();
                    async move {
                        request.extensions_mut().insert(ext);
                        next.run(request).await
                    }
                },
            ));

        let response = app
            .oneshot(chat_request(GPT4O_RAW))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(available(&ledger).await, 1_000_000);
    }

    #[tokio::test]
    async fn request_max_tokens_sizes_the_hold() {
        let body = json!({
            "model": GPT4O_RAW,
            "messages": [],
            "max_completion_tokens": 50_000
        })
        .to_string();
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .body(Body::from(body))
            .expect("request");
        let (_rebuilt, pricing) = buffer_request_pricing(request)
            .await
            .expect("request pricing");
        assert_eq!(pricing.completion_token_ceiling, Some(50_000));
        assert_eq!(
            EstimatePolicy::default().hold_estimate(&price_table(), &pricing),
            Micros::new(510_000)
        );
    }

    #[tokio::test]
    async fn successful_over_budget_response_captures_authorized_hold_not_free() {
        let ledger = seeded_ledger(1_000_000).await;
        let state = BillingState::new(
            ledger.clone(),
            EstimatePolicy {
                hold_prompt_tokens: 1,
                hold_completion_tokens: 1,
            },
            price_table(),
            DEFAULT_AITP_PAYEE,
        );
        let app = app_with_account(state, key_extension(ACCOUNT), GPT4O_RAW);
        let body = json!({ "model": GPT4O_RAW, "messages": [] }).to_string();
        let prompt_ceiling = body.len() as u32;
        let authorized = price_table().cost(&gpt4o(), prompt_ceiling, 1).get();
        let request = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("content-type", "application/json")
            .body(Body::from(body))
            .expect("request");

        let response = app.oneshot(request).await.expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get("x-relay-billing-overage")
                .and_then(|value| value.to_str().ok()),
            Some("capped-to-reservation")
        );
        assert_eq!(available(&ledger).await, 1_000_000 - authorized);
        assert!(authorized > 0, "successful call must never settle for free");
    }
}
