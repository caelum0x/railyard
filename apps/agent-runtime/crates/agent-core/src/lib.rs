//! `agent-core`: the base agent runtime for the agent-economy apps
//! (Turnstile / Abacus).
//!
//! It ties three pieces together:
//!   * rig's [`Agent`](rig_core::agent::Agent) runtime,
//!   * a completion model (typically [`relay_provider::RelayModel`] over Relay),
//!   * either Relay-owned dynamic AITP-402 recovery in the model's HTTP client,
//!     or the fixed-price direct [`PaidCompletionModel`] compatibility seam.
//!
//! Nothing here imports Mint. The two payment compositions are alternatives
//! and must not wrap each other.

pub mod agent;
pub mod paid;
pub mod journal;
pub mod reconcile;
pub mod runtime;

pub use agent::RuntimeAgent;
pub use paid::{PaidCompletionModel, PricedRoute};
pub use reconcile::{
    CaptureReconciler, DeadLetteredCapture, PendingCapture, ReconcilerConfig,
};
pub use runtime::{
    build_agent, build_paid_agent, build_paid_agent_with_reconciler, RuntimeConfig,
};

// Re-export the seams so downstream apps depend on one crate.
pub use aitp_client;
pub use relay_provider;

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use aitp_client::{
        AmountMicros, Authorized, HoldRef, PayError, PaymentChallenge, PaymentGateway, Receipt,
        StubPaymentGateway,
    };
    use futures_util::StreamExt;
    use rig_core::completion::Prompt;
    use rig_core::test_utils::{MockCompletionModel, MockStreamEvent};

    use super::*;

    fn route() -> PricedRoute {
        PricedRoute::new(
            "ait1qprovider",
            AmountMicros(4200),
            "POST /v1/chat/completions#mock",
        )
        .expect("valid priced route")
    }

    struct AmbiguousCaptureGateway {
        inner: StubPaymentGateway,
        capture_attempts: AtomicUsize,
    }

    impl Default for AmbiguousCaptureGateway {
        fn default() -> Self {
            Self {
                inner: StubPaymentGateway::default(),
                capture_attempts: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl PaymentGateway for AmbiguousCaptureGateway {
        async fn authorize(&self, challenge: &PaymentChallenge) -> Result<Authorized, PayError> {
            self.inner.authorize(challenge).await
        }

        async fn capture(&self, hold: &HoldRef, actual: AmountMicros) -> Result<Receipt, PayError> {
            let receipt = self.inner.capture(hold, actual).await?;
            if self.capture_attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(PayError::Gateway("capture response was lost".into()));
            }
            Ok(receipt)
        }

        async fn release(&self, hold: &HoldRef) -> Result<(), PayError> {
            self.inner.release(hold).await
        }
    }

    #[tokio::test]
    async fn paid_agent_pays_and_replies_offline() {
        let model = MockCompletionModel::text("agent says hi");
        let gateway = Arc::new(StubPaymentGateway::default());
        let agent = build_paid_agent(
            model,
            gateway.clone(),
            route(),
            &RuntimeConfig {
                preamble: Some("you are a test agent".into()),
                ..Default::default()
            },
        );

        let reply = agent.prompt("hello").await.expect("agent replies");
        assert_eq!(reply, "agent says hi");

        // The payment seam ran: authorize then capture, no release on success.
        gateway.with_calls(|c| {
            assert_eq!(c.authorized.len(), 1);
            assert_eq!(c.captured.len(), 1);
            assert_eq!(c.released.len(), 0);
            assert_eq!(c.captured[0].1, AmountMicros(4200));
        });
    }

    #[tokio::test]
    async fn ambiguous_buffered_capture_retries_capture_and_never_releases() {
        let model = MockCompletionModel::text("already served");
        let gateway = Arc::new(AmbiguousCaptureGateway::default());
        let agent = RuntimeAgent::paid(model, gateway.clone(), route(), &RuntimeConfig::default());

        let result = agent.run("hello").await;
        assert!(result.is_err(), "unsettled success must fail closed");
        for _ in 0..1_000 {
            if gateway.capture_attempts.load(Ordering::SeqCst) >= 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(gateway.capture_attempts.load(Ordering::SeqCst), 2);
        gateway.inner.with_calls(|calls| {
            assert_eq!(calls.captured.len(), 2);
            assert!(calls.released.is_empty());
        });
    }

    #[tokio::test]
    async fn runtime_agent_run_settles_a_hold_around_inference() {
        let model = MockCompletionModel::text("runtime agent reply");
        let gateway = Arc::new(StubPaymentGateway::default());
        let agent = RuntimeAgent::paid(model, gateway.clone(), route(), &RuntimeConfig::default());

        let reply = agent.run("hello").await.expect("agent replies");
        assert_eq!(reply, "runtime agent reply");

        // A hold was authorized before and captured after the inference.
        gateway.with_calls(|c| {
            assert_eq!(c.authorized.len(), 1);
            assert_eq!(c.captured.len(), 1);
            assert_eq!(c.released.len(), 0);
        });
    }

    #[tokio::test]
    async fn runtime_agent_run_works_without_payment() {
        let model = MockCompletionModel::text("free reply");
        let agent = RuntimeAgent::new(model, &RuntimeConfig::default());
        let reply = agent.run("hello").await.expect("agent replies");
        assert_eq!(reply, "free reply");
    }

    #[tokio::test]
    async fn runtime_agent_exposes_incremental_streaming() {
        let model = MockCompletionModel::from_stream_turns([[
            MockStreamEvent::text("hel"),
            MockStreamEvent::text("lo"),
            MockStreamEvent::final_response_with_total_tokens(7),
        ]]);
        let agent = RuntimeAgent::new(model, &RuntimeConfig::default());
        let mut stream = agent.stream("hello").await;
        let mut text = String::new();
        let mut saw_final = false;
        while let Some(item) = stream.next().await {
            match item.expect("stream item") {
                rig_core::agent::MultiTurnStreamItem::StreamAssistantItem(
                    rig_core::streaming::StreamedAssistantContent::Text(chunk),
                ) => text.push_str(&chunk.text),
                rig_core::agent::MultiTurnStreamItem::FinalResponse(_) => saw_final = true,
                _ => {}
            }
        }
        assert_eq!(text, "hello");
        assert!(saw_final);
    }

    #[tokio::test]
    async fn direct_paid_agent_stream_authorizes_then_captures_at_terminal_response() {
        let model = MockCompletionModel::from_stream_turns([[
            MockStreamEvent::text("hel"),
            MockStreamEvent::text("lo"),
            MockStreamEvent::final_response_with_total_tokens(7),
        ]]);
        let gateway = Arc::new(StubPaymentGateway::default());
        let agent = RuntimeAgent::paid(model, gateway.clone(), route(), &RuntimeConfig::default());
        let mut stream = agent.stream("hello").await;
        let mut text = String::new();
        let mut saw_final = false;

        while let Some(item) = stream.next().await {
            match item.expect("paid stream item") {
                rig_core::agent::MultiTurnStreamItem::StreamAssistantItem(
                    rig_core::streaming::StreamedAssistantContent::Text(chunk),
                ) => text.push_str(&chunk.text),
                rig_core::agent::MultiTurnStreamItem::FinalResponse(_) => saw_final = true,
                _ => {}
            }
        }

        assert_eq!(text, "hello");
        assert!(saw_final);
        gateway.with_calls(|calls| {
            assert_eq!(calls.authorized.len(), 1);
            assert_eq!(calls.captured.len(), 1);
            assert_eq!(calls.captured[0].1, AmountMicros(4200));
            assert!(calls.released.is_empty());
        });
    }

    #[tokio::test]
    async fn ambiguous_stream_capture_retries_capture_and_never_releases() {
        let model = MockCompletionModel::from_stream_turns([[
            MockStreamEvent::text("served"),
            MockStreamEvent::final_response_with_total_tokens(7),
        ]]);
        let gateway = Arc::new(AmbiguousCaptureGateway::default());
        let agent = RuntimeAgent::paid(model, gateway.clone(), route(), &RuntimeConfig::default());
        let mut stream = agent.stream("hello").await;
        let mut saw_error = false;
        while let Some(item) = stream.next().await {
            if item.is_err() {
                saw_error = true;
            }
        }

        assert!(saw_error, "terminal evidence must wait for capture");
        for _ in 0..1_000 {
            if gateway.capture_attempts.load(Ordering::SeqCst) >= 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(gateway.capture_attempts.load(Ordering::SeqCst), 2);
        gateway.inner.with_calls(|calls| {
            assert_eq!(calls.captured.len(), 2);
            assert!(calls.released.is_empty());
        });
    }

    #[tokio::test]
    async fn direct_paid_agent_stream_error_releases_without_capture() {
        let model = MockCompletionModel::from_stream_turns([[
            MockStreamEvent::text("partial"),
            MockStreamEvent::error("provider disconnected"),
        ]]);
        let gateway = Arc::new(StubPaymentGateway::default());
        let agent = RuntimeAgent::paid(model, gateway.clone(), route(), &RuntimeConfig::default());
        let mut stream = agent.stream("hello").await;
        let mut saw_error = false;
        while let Some(item) = stream.next().await {
            if item.is_err() {
                saw_error = true;
            }
        }

        assert!(saw_error);
        gateway.with_calls(|calls| {
            assert_eq!(calls.authorized.len(), 1);
            assert!(calls.captured.is_empty());
            assert_eq!(calls.released.len(), 1);
        });
    }

    #[tokio::test]
    async fn direct_paid_agent_stream_creation_failure_releases_hold() {
        let model = MockCompletionModel::default();
        let gateway = Arc::new(StubPaymentGateway::default());
        let agent = RuntimeAgent::paid(model, gateway.clone(), route(), &RuntimeConfig::default());
        let mut stream = agent.stream("hello").await;
        let mut saw_error = false;
        while let Some(item) = stream.next().await {
            if item.is_err() {
                saw_error = true;
            }
        }

        assert!(saw_error);
        gateway.with_calls(|calls| {
            assert_eq!(calls.authorized.len(), 1);
            assert!(calls.captured.is_empty());
            assert_eq!(calls.released.len(), 1);
        });
    }

    #[tokio::test]
    async fn direct_paid_agent_stream_missing_terminal_releases_and_fails_closed() {
        let model = MockCompletionModel::from_stream_turns([[MockStreamEvent::text("incomplete")]]);
        let gateway = Arc::new(StubPaymentGateway::default());
        let agent = RuntimeAgent::paid(model, gateway.clone(), route(), &RuntimeConfig::default());
        let mut stream = agent.stream("hello").await;
        let mut saw_error = false;
        while let Some(item) = stream.next().await {
            if item.is_err() {
                saw_error = true;
            }
        }

        assert!(saw_error);
        gateway.with_calls(|calls| {
            assert!(calls.captured.is_empty());
            assert_eq!(calls.released.len(), 1);
        });
    }

    #[tokio::test]
    async fn dropping_direct_paid_stream_reconciles_release_in_background() {
        let model = MockCompletionModel::from_stream_turns([[
            MockStreamEvent::text("partial"),
            MockStreamEvent::text("unread"),
            MockStreamEvent::final_response_with_total_tokens(7),
        ]]);
        let gateway = Arc::new(StubPaymentGateway::default());
        let agent = RuntimeAgent::paid(model, gateway.clone(), route(), &RuntimeConfig::default());
        let mut stream = agent.stream("hello").await;
        assert!(stream.next().await.is_some());
        drop(stream);

        for _ in 0..1_000 {
            if gateway.with_calls(|calls| !calls.released.is_empty()) {
                break;
            }
            tokio::task::yield_now().await;
        }
        gateway.with_calls(|calls| {
            assert!(calls.captured.is_empty());
            assert_eq!(calls.released.len(), 1);
        });
    }

    #[tokio::test]
    async fn runtime_agent_run_releases_hold_on_payment_rejection() {
        let model = MockCompletionModel::text("never reached");
        let gateway = Arc::new(StubPaymentGateway::always_failing(|| {
            PayError::InsufficientFunds
        }));
        let agent = RuntimeAgent::paid(model, gateway, route(), &RuntimeConfig::default());
        let result = agent.run("hello").await;
        assert!(result.is_err(), "insufficient funds must fail run()");
    }

    #[tokio::test]
    async fn build_paid_agent_with_reconciler_exposes_owed_captures_for_shutdown_drain() {
        // First capture fails after inference is served (revenue owed), then the
        // background reconciler retries and settles. The builder handle must let
        // an operator observe that owed-capture lifecycle without the model.
        let model = MockCompletionModel::text("already served");
        let gateway = Arc::new(AmbiguousCaptureGateway::default());
        let (agent, reconciler) = build_paid_agent_with_reconciler(
            model,
            gateway.clone(),
            route(),
            &RuntimeConfig::default(),
        );

        let result = agent.prompt("hello").await;
        assert!(result.is_err(), "an unsettled capture must fail closed");

        // The owed capture funnels into the builder-exposed queue and settles on
        // retry — reachable entirely through the returned handle.
        for _ in 0..2_000 {
            if reconciler.pending_len() == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(reconciler.pending_len(), 0, "owed capture reconciled via handle");
        assert_eq!(reconciler.dead_letter_len(), 0, "no revenue was abandoned");
        gateway.inner.with_calls(|calls| {
            assert_eq!(calls.captured.len(), 2, "capture retried until it settled");
            assert!(calls.released.is_empty());
        });
    }

    #[tokio::test]
    async fn payment_rejection_fails_the_completion() {
        let model = MockCompletionModel::text("never reached");
        let gateway = Arc::new(StubPaymentGateway::always_failing(|| {
            PayError::InsufficientFunds
        }));
        let agent = build_paid_agent(model, gateway, route(), &RuntimeConfig::default());

        let result = agent.prompt("hello").await;
        assert!(result.is_err(), "insufficient funds must fail the prompt");
    }
}
