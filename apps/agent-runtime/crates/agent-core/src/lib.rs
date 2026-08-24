//! `agent-core`: the base agent runtime for the agent-economy apps
//! (Turnstile / Abacus).
//!
//! It ties three pieces together:
//!   * rig's [`Agent`](rig_agent::Agent) runtime (vendored `external/rig`),
//!   * a completion model (typically [`relay_provider::RelayModel`] over Relay),
//!   * the AITP-402 payment seam ([`aitp_client::PaymentGateway`]) wired as the
//!     [`PaidCompletionModel`] decorator.
//!
//! Nothing here imports Mint: payment stays behind the trait.

pub mod agent;
pub mod paid;
pub mod runtime;

pub use agent::RuntimeAgent;
pub use paid::{PaidCompletionModel, PricedRoute};
pub use runtime::{build_agent, build_paid_agent, RuntimeConfig};

// Re-export the seams so downstream apps depend on one crate.
pub use aitp_client;
pub use relay_provider;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use aitp_client::{AmountMicros, PayError, StubPaymentGateway};
    use rig_agent::completion::Prompt;
    use rig_core::test_utils::MockCompletionModel;

    use super::*;

    fn route() -> PricedRoute {
        PricedRoute::new(
            "ait1qprovider",
            AmountMicros(4200),
            "POST /v1/chat/completions#mock",
        )
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
