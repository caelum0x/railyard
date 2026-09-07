//! [`PaidCompletionModel`] — fixed-price direct-Mint compatibility decorator.
//!
//! This legacy-compatible path wraps any inner [`CompletionModel`] and runs a
//! locally-priced hold/capture cycle against a [`PaymentGateway`]. It must not
//! wrap a Relay-billed model: Relay's `402` transport already owns dynamic
//! reservation and exact usage settlement, and composing both would double
//! charge the request.
//!
//! 1. derive/obtain a [`PaymentChallenge`] for the priced route,
//! 2. `gateway.authorize` (aitp-hold),
//! 3. delegate to the inner model,
//! 4. on success `gateway.capture` the configured fixed price; on error `gateway.release`
//!    (success-only billing, SPEC ADR-0002).
//!
//! New Relay callers should use `RelayModel::connect_paid` and
//! `RuntimeAgent::new`. This decorator remains for direct endpoints whose exact
//! fixed per-call price is operator-owned. With a `StubPaymentGateway` + a mock
//! model the compatibility flow still runs offline.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aitp_client::{AmountMicros, HoldRef, PayError, PaymentChallenge, PaymentGateway, Scheme};
use futures_util::StreamExt;
use rig_core::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse,
};
use rig_core::streaming::{
    RawStreamingChoice, RawStreamingToolCall, StreamedAssistantContent,
    StreamingCompletionResponse, StreamingResult,
};
use rig_core::wasm_compat::WasmCompatSend;

use crate::reconcile::CaptureReconciler;

#[derive(Debug, Clone, Copy)]
enum PendingSettlement {
    Release,
    Capture(AmountMicros),
}

/// Replays the terminal money intent if a consumer drops a stream mid-flight.
///
/// Direct paid streaming has no server-side outbox. The guard keeps the one safe
/// idempotent command alive: before a terminal model response that command is
/// `release`; once success is proven it becomes `capture` and can never flip
/// back to release. A dropped-mid-flight `release` is best-effort (a lost
/// release only leaves a hold to expire — the payer is never overcharged). A
/// pending `capture`, however, is *revenue owed*, so it is handed to the durable
/// [`CaptureReconciler`] instead of a fire-and-forget spawn.
struct StreamSettlementGuard<P>
where
    P: PaymentGateway + 'static,
{
    gateway: Arc<P>,
    reconciler: Arc<CaptureReconciler<P>>,
    hold: HoldRef,
    pending: Option<PendingSettlement>,
}

impl<P> StreamSettlementGuard<P>
where
    P: PaymentGateway + 'static,
{
    fn new(gateway: Arc<P>, reconciler: Arc<CaptureReconciler<P>>, hold: HoldRef) -> Self {
        Self {
            gateway,
            reconciler,
            hold,
            pending: Some(PendingSettlement::Release),
        }
    }

    fn require_capture(&mut self, amount: AmountMicros) {
        self.pending = Some(PendingSettlement::Capture(amount));
    }

    fn disarm(&mut self) {
        self.pending = None;
    }
}

impl<P> Drop for StreamSettlementGuard<P>
where
    P: PaymentGateway + 'static,
{
    fn drop(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        match pending {
            // Revenue owed: route to the durable reconciliation queue, which
            // retries until Mint confirms or explicitly dead-letters. Idempotent
            // per hold, so an explicit-path enqueue plus this safety-net enqueue
            // never double-charge.
            PendingSettlement::Capture(amount) => {
                self.reconciler.enqueue(self.hold.clone(), amount);
            }
            // Best-effort: a lost release only leaves a hold to expire.
            PendingSettlement::Release => {
                let gateway = self.gateway.clone();
                let hold = self.hold.clone();
                let Ok(runtime) = tokio::runtime::Handle::try_current() else {
                    tracing::error!(
                        hold = ?hold,
                        "AITP-402 stream dropped outside a Tokio runtime; \
                         hold release needs reconciliation"
                    );
                    return;
                };
                runtime.spawn(async move {
                    if let Err(error) = gateway.release(&hold).await {
                        tracing::error!(
                            hold = ?hold,
                            %error,
                            "AITP-402 dropped-stream release replay failed"
                        );
                    }
                });
            }
        }
    }
}

/// A direct endpoint with one fixed operator-owned price.
#[derive(Debug, Clone)]
pub struct PricedRoute {
    /// Provider receive address (`ait1…`).
    pub payee: String,
    /// Maximum reserved per request, in microdollars.
    pub price: AmountMicros,
    /// Resource identifier, e.g. `POST /v1/chat/completions#<model>`.
    pub resource: String,
}

impl PricedRoute {
    /// Build a priced route, rejecting a non-positive price.
    ///
    /// A `price <= 0` would authorize and capture nothing, mirroring the Relay
    /// path's `max_amount_micros > 0` check in `relay_provider::config`. Money
    /// intent is validated at the boundary so a misconfigured route fails fast
    /// instead of silently billing zero.
    pub fn new(
        payee: impl Into<String>,
        price: AmountMicros,
        resource: impl Into<String>,
    ) -> Result<Self, PayError> {
        if price.get() <= 0 {
            return Err(PayError::Policy(
                "priced route price must be greater than zero".into(),
            ));
        }
        Ok(Self {
            payee: payee.into(),
            price,
            resource: resource.into(),
        })
    }

    fn challenge(&self, nonce: String) -> PaymentChallenge {
        let expiry = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64 + 60_000)
            .unwrap_or(0);
        PaymentChallenge {
            payee: self.payee.clone(),
            amount_micros: self.price,
            currency: "AIT".to_string(),
            resource: self.resource.clone(),
            nonce,
            expiry,
            scheme: Scheme::Hold,
        }
    }
}

/// Payment-decorated completion model.
pub struct PaidCompletionModel<M, P> {
    inner: M,
    gateway: Arc<P>,
    route: PricedRoute,
    nonce_seq: Arc<AtomicU64>,
    /// Durable-ish retry queue for captures that fail *after* inference was
    /// served. Shared across clones so every request funnels into one queue.
    reconciler: Arc<CaptureReconciler<P>>,
}

impl<M: Clone, P> Clone for PaidCompletionModel<M, P> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            gateway: self.gateway.clone(),
            route: self.route.clone(),
            nonce_seq: self.nonce_seq.clone(),
            reconciler: self.reconciler.clone(),
        }
    }
}

impl<M, P> PaidCompletionModel<M, P>
where
    P: PaymentGateway + 'static,
{
    pub fn new(inner: M, gateway: Arc<P>, route: PricedRoute) -> Self {
        let reconciler = Arc::new(CaptureReconciler::with_defaults(gateway.clone()));
        Self {
            inner,
            gateway,
            route,
            nonce_seq: Arc::new(AtomicU64::new(1)),
            reconciler,
        }
    }

    /// Handle to the shared capture-reconciliation queue, for shutdown draining
    /// (`reconcile_now`) and dead-letter inspection.
    pub fn reconciler(&self) -> Arc<CaptureReconciler<P>> {
        self.reconciler.clone()
    }
}

impl<M, P> PaidCompletionModel<M, P> {
    fn next_nonce(&self) -> String {
        let unix_nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        format!(
            "nonce_{unix_nanos:x}_{:x}_{}",
            std::process::id(),
            self.nonce_seq.fetch_add(1, Ordering::Relaxed)
        )
    }
}

impl<M, P> CompletionModel for PaidCompletionModel<M, P>
where
    M: CompletionModel,
    M::StreamingResponse: 'static,
    P: PaymentGateway + 'static,
{
    type Response = M::Response;
    type StreamingResponse = M::StreamingResponse;
    type Client = (M::Client, Arc<P>, PricedRoute);

    fn make(client: &Self::Client, model: impl Into<String>) -> Self {
        Self::new(
            M::make(&client.0, model),
            client.1.clone(),
            client.2.clone(),
        )
    }

    fn completion(
        &self,
        request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<CompletionResponse<Self::Response>, CompletionError>>
           + WasmCompatSend {
        let challenge = self.route.challenge(self.next_nonce());
        let price = self.route.price;
        async move {
            let authorized = self.gateway.authorize(&challenge).await.map_err(|e| {
                CompletionError::ProviderError(format!("aitp-402 authorize failed: {e}"))
            })?;
            let mut settlement = authorized.hold.as_ref().map(|hold| {
                StreamSettlementGuard::new(
                    self.gateway.clone(),
                    self.reconciler.clone(),
                    hold.clone(),
                )
            });

            match self.inner.completion(request).await {
                Ok(response) => {
                    if let Some(hold) = &authorized.hold {
                        // This compatibility path is intentionally fixed-price.
                        // Dynamic token pricing belongs to Relay, which must use
                        // `RelayModel::connect_paid` without this decorator.
                        //
                        // The provider has already served this request. A failed
                        // capture is ambiguous and must never be followed by the
                        // opposite terminal command: capture may have committed.
                        // Return an error and retain the hold for idempotent
                        // capture reconciliation instead of releasing successful
                        // inference for free.
                        settlement
                            .as_mut()
                            .expect("hold settlement guard must exist")
                            .require_capture(price);
                        if let Err(e) = self.gateway.capture(hold, price).await {
                            // The provider already served this request. A failed
                            // capture is money owed, not free inference. Hand the
                            // hold to the durable reconciler (bounded retries →
                            // capture-or-dead-letter) instead of losing it to a
                            // single fire-and-forget replay, then disarm the guard
                            // so it does not enqueue a duplicate.
                            self.reconciler.enqueue(hold.clone(), price);
                            settlement
                                .as_mut()
                                .expect("hold settlement guard must exist")
                                .disarm();
                            tracing::error!(
                                hold = ?hold,
                                amount = ?price,
                                error = %e,
                                "AITP-402 capture failed after inference was served; \
                                 queued for durable capture reconciliation"
                            );
                            return Err(CompletionError::ProviderError(format!(
                                "AITP-402 capture pending after completion: {e}"
                            )));
                        }
                        settlement
                            .as_mut()
                            .expect("hold settlement guard must exist")
                            .disarm();
                    }
                    Ok(response)
                }
                Err(err) => {
                    // Failure path: release the hold so the agent pays nothing.
                    if let Some(hold) = &authorized.hold {
                        if self.gateway.release(hold).await.is_ok() {
                            settlement
                                .as_mut()
                                .expect("hold settlement guard must exist")
                                .disarm();
                        }
                    }
                    Err(err)
                }
            }
        }
    }

    fn stream(
        &self,
        request: CompletionRequest,
    ) -> impl std::future::Future<
        Output = Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError>,
    > + WasmCompatSend {
        let challenge = self.route.challenge(self.next_nonce());
        let price = self.route.price;
        async move {
            let authorized = self.gateway.authorize(&challenge).await.map_err(|error| {
                CompletionError::ProviderError(format!(
                    "AITP-402 stream authorization failed: {error}"
                ))
            })?;
            let Some(hold) = authorized.hold else {
                return Err(CompletionError::ProviderError(
                    "AITP-402 fixed-price streaming requires a hold authorization".to_string(),
                ));
            };

            let mut settlement = StreamSettlementGuard::new(
                self.gateway.clone(),
                self.reconciler.clone(),
                hold.clone(),
            );
            let mut provider_stream = match self.inner.stream(request).await {
                Ok(stream) => stream,
                Err(error) => {
                    match self.gateway.release(&hold).await {
                        Ok(()) => settlement.disarm(),
                        Err(release_error) => {
                            tracing::error!(
                                hold = ?hold,
                                %release_error,
                                "AITP-402 release failed after stream creation failed"
                            );
                        }
                    }
                    return Err(error);
                }
            };
            let gateway = self.gateway.clone();
            let reconciler = self.reconciler.clone();
            let stream = async_stream::stream! {
                while let Some(item) = provider_stream.next().await {
                    let choice = match item {
                        Ok(choice) => choice,
                        Err(error) => {
                            if gateway.release(&hold).await.is_ok() {
                                settlement.disarm();
                            }
                            yield Err(error);
                            return;
                        }
                    };
                    match choice {
                        StreamedAssistantContent::Text(text) => {
                            yield Ok(RawStreamingChoice::TextStart {
                                additional_params: text.additional_params,
                            });
                            yield Ok(RawStreamingChoice::Message(text.text));
                        }
                        StreamedAssistantContent::ToolCall {
                            tool_call,
                            internal_call_id,
                        } => {
                            yield Ok(RawStreamingChoice::ToolCall(RawStreamingToolCall {
                                id: tool_call.id,
                                internal_call_id,
                                call_id: tool_call.call_id,
                                name: tool_call.function.name,
                                arguments: tool_call.function.arguments,
                                signature: tool_call.signature,
                                additional_params: tool_call.additional_params,
                            }));
                        }
                        StreamedAssistantContent::ToolCallDelta {
                            id,
                            internal_call_id,
                            content,
                        } => {
                            yield Ok(RawStreamingChoice::ToolCallDelta {
                                id,
                                internal_call_id,
                                content,
                            });
                        }
                        StreamedAssistantContent::Reasoning(reasoning) => {
                            for content in reasoning.content {
                                yield Ok(RawStreamingChoice::Reasoning {
                                    id: reasoning.id.clone(),
                                    content,
                                });
                            }
                        }
                        StreamedAssistantContent::ReasoningDelta { id, reasoning } => {
                            yield Ok(RawStreamingChoice::ReasoningDelta { id, reasoning });
                        }
                        StreamedAssistantContent::Final(response) => {
                            settlement.require_capture(price);
                            match gateway.capture(&hold, price).await {
                                Ok(_) => {
                                    settlement.disarm();
                                    if let Some(message_id) = provider_stream.message_id.clone() {
                                        yield Ok(RawStreamingChoice::MessageId(message_id));
                                    }
                                    yield Ok(RawStreamingChoice::FinalResponse(response));
                                }
                                Err(error) => {
                                    // Inference was served; the failed capture is
                                    // revenue owed. Enqueue durable reconciliation
                                    // and disarm the guard so it does not enqueue
                                    // a duplicate.
                                    reconciler.enqueue(hold.clone(), price);
                                    settlement.disarm();
                                    yield Err(CompletionError::ProviderError(format!(
                                        "AITP-402 capture pending after stream completion: {error}"
                                    )));
                                }
                            }
                            return;
                        }
                    }
                }

                let release = gateway.release(&hold).await;
                if release.is_ok() {
                    settlement.disarm();
                }
                let detail = release
                    .err()
                    .map(|error| format!("; release pending: {error}"))
                    .unwrap_or_default();
                yield Err(CompletionError::ProviderError(format!(
                    "AITP-402 provider stream ended without a terminal response{detail}"
                )));
            };
            let stream: StreamingResult<Self::StreamingResponse> = Box::pin(stream);
            Ok(StreamingCompletionResponse::stream(stream))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use aitp_client::StubPaymentGateway;

    use super::*;

    fn hold() -> HoldRef {
        HoldRef("hold_guard_test".into())
    }

    #[test]
    fn priced_route_rejects_non_positive_price() {
        assert!(PricedRoute::new("ait1qprovider", AmountMicros(1), "POST /x").is_ok());
        assert!(matches!(
            PricedRoute::new("ait1qprovider", AmountMicros(0), "POST /x"),
            Err(PayError::Policy(_))
        ));
        assert!(matches!(
            PricedRoute::new("ait1qprovider", AmountMicros(-5), "POST /x"),
            Err(PayError::Policy(_))
        ));
    }

    /// Dropping a fixed-price stream *after* `require_capture` is revenue owed:
    /// the guard must hand the capture to the durable reconciler (never a lost
    /// fire-and-forget), which then settles it.
    #[tokio::test]
    async fn dropping_guard_after_require_capture_enqueues_durable_capture() {
        let gateway = Arc::new(StubPaymentGateway::default());
        let reconciler = Arc::new(CaptureReconciler::with_defaults(gateway.clone()));
        let mut guard =
            StreamSettlementGuard::new(gateway.clone(), reconciler.clone(), hold());
        guard.require_capture(AmountMicros(4200));
        drop(guard);

        for _ in 0..2_000 {
            if reconciler.pending_len() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(reconciler.pending_len(), 0, "owed capture must settle");
        assert_eq!(reconciler.dead_letter_len(), 0, "settled capture is not dead-lettered");
        gateway.with_calls(|calls| {
            assert_eq!(calls.captured.len(), 1, "the owed price was captured exactly once");
            assert_eq!(calls.captured[0].1, AmountMicros(4200));
            assert!(calls.released.is_empty(), "revenue owed is never released");
        });
    }

    /// Dropping before any terminal response schedules a best-effort release —
    /// a lost release only leaves a hold to expire; the payer is never charged.
    #[tokio::test]
    async fn dropping_guard_before_terminal_schedules_best_effort_release() {
        let gateway = Arc::new(StubPaymentGateway::default());
        let reconciler = Arc::new(CaptureReconciler::with_defaults(gateway.clone()));
        let guard = StreamSettlementGuard::new(gateway.clone(), reconciler.clone(), hold());
        // `pending` defaults to Release; drop it without proving success.
        drop(guard);

        for _ in 0..2_000 {
            if gateway.with_calls(|calls| !calls.released.is_empty()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        gateway.with_calls(|calls| {
            assert!(calls.captured.is_empty(), "no capture without a terminal response");
            assert_eq!(calls.released.len(), 1, "the hold is released best-effort");
        });
        assert_eq!(reconciler.pending_len(), 0, "release never enters the capture queue");
        assert_eq!(reconciler.dead_letter_len(), 0);
    }

    /// A disarmed guard (terminal settlement already resolved) does nothing on
    /// drop — no duplicate capture and no spurious release.
    #[tokio::test]
    async fn disarmed_guard_is_inert_on_drop() {
        let gateway = Arc::new(StubPaymentGateway::default());
        let reconciler = Arc::new(CaptureReconciler::with_defaults(gateway.clone()));
        let mut guard = StreamSettlementGuard::new(gateway.clone(), reconciler.clone(), hold());
        guard.require_capture(AmountMicros(4200));
        guard.disarm();
        drop(guard);

        // Give any erroneously-spawned task a chance to run.
        for _ in 0..50 {
            tokio::task::yield_now().await;
        }
        gateway.with_calls(|calls| {
            assert!(calls.captured.is_empty());
            assert!(calls.released.is_empty());
        });
        assert_eq!(reconciler.pending_len(), 0);
    }
}
