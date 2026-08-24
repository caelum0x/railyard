//! [`PaidCompletionModel`] — the AITP-402 payment seam, wired as a rig
//! [`CompletionModel`] decorator.
//!
//! Per the recon dep strategy, payment composes orthogonally to rig's model
//! abstraction: this decorator wraps any inner [`CompletionModel`] (the Relay
//! model in production, a mock in tests) and, on each call, runs the caller
//! side of the handshake against a [`PaymentGateway`]:
//!
//! 1. derive/obtain a [`PaymentChallenge`] for the priced route,
//! 2. `gateway.authorize` (aitp-hold),
//! 3. delegate to the inner model,
//! 4. on success `gateway.capture` the actual spend; on error `gateway.release`
//!    (success-only billing, SPEC ADR-0002).
//!
//! Turnstile/Abacus therefore build `AgentBuilder::new(PaidCompletionModel::new(
//! relay_model, gateway, route))` and get payment for free. With a
//! `StubPaymentGateway` + a mock model this whole flow runs offline.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use aitp_client::{AmountMicros, PaymentChallenge, PaymentGateway, Scheme};
use rig_core::completion::{
    CompletionError, CompletionModel, CompletionRequest, CompletionResponse, ProviderCapabilities,
};
use rig_core::streaming::StreamingCompletionResponse;
use rig_core::wasm_compat::WasmCompatSend;

/// A priced resource behind AITP-402. In production the challenge comes from
/// Relay's `402` response; until Relay billing is switched on this describes the
/// price/route locally so the seam is fully exercised.
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
    pub fn new(payee: impl Into<String>, price: AmountMicros, resource: impl Into<String>) -> Self {
        Self {
            payee: payee.into(),
            price,
            resource: resource.into(),
        }
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
#[derive(Clone)]
pub struct PaidCompletionModel<M, P> {
    inner: M,
    gateway: Arc<P>,
    route: PricedRoute,
    nonce_seq: Arc<AtomicU64>,
}

impl<M, P> PaidCompletionModel<M, P> {
    pub fn new(inner: M, gateway: Arc<P>, route: PricedRoute) -> Self {
        Self {
            inner,
            gateway,
            route,
            nonce_seq: Arc::new(AtomicU64::new(1)),
        }
    }

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
    P: PaymentGateway + 'static,
{
    fn completion(
        &self,
        request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<CompletionResponse, CompletionError>> + WasmCompatSend
    {
        let challenge = self.route.challenge(self.next_nonce());
        let price = self.route.price;
        async move {
            let authorized = self.gateway.authorize(&challenge).await.map_err(|e| {
                CompletionError::ProviderError(format!("aitp-402 authorize failed: {e}"))
            })?;

            match self.inner.completion(request).await {
                Ok(response) => {
                    if let Some(hold) = &authorized.hold {
                        // TODO: map actual token usage (response.usage) to
                        // microdollars once Relay reports per-request pricing;
                        // reserve price is captured for now.
                        //
                        // The provider has ALREADY served this request, so a
                        // settle failure must NOT be turned into a user-facing
                        // error (that would drop a delivered completion). Instead
                        // best-effort release the hold so the reservation never
                        // leaks, log for out-of-band reconciliation, and still
                        // return the response.
                        if let Err(e) = self.gateway.capture(hold, price).await {
                            let released = self.gateway.release(hold).await;
                            tracing::error!(
                                hold = ?hold,
                                amount = ?price,
                                error = %e,
                                released = released.is_ok(),
                                "aitp-402 capture failed after inference was served; \
                                 hold released best-effort, needs reconciliation"
                            );
                        }
                    }
                    Ok(response)
                }
                Err(err) => {
                    // Failure path: release the hold so the agent pays nothing.
                    if let Some(hold) = &authorized.hold {
                        let _ = self.gateway.release(hold).await;
                    }
                    Err(err)
                }
            }
        }
    }

    fn stream(
        &self,
        _request: CompletionRequest,
    ) -> impl std::future::Future<Output = Result<StreamingCompletionResponse, CompletionError>>
           + WasmCompatSend {
        // Fail closed: the streaming surface is reachable via
        // `RuntimeAgent::agent() -> &Agent` (rig's streaming runner calls
        // `model.stream()`). Delegating straight to `inner.stream` would serve
        // completely unbilled inference (payment bypass). Until the metered
        // authorize->stream->capture/release loop is wired, refuse rather than
        // give away free inference.
        async move {
            Err(CompletionError::ProviderError(
                "AITP-402: streaming not yet metered".to_string(),
            ))
        }
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.inner.capabilities()
    }
}
