//! [`RuntimeAgent`] — a thin, ergonomic wrapper over a rig [`Agent`] that
//! exposes a single `run(prompt)` entry point for the economy apps
//! (Turnstile / Abacus).
//!
//! Payment can be composed in exactly one of two places. A
//! `relay_provider::RelayModel::connect_paid` model answers Relay's dynamic
//! AITP-402 challenge below rig's HTTP client, leaving Relay as settlement
//! owner. The older [`PaidCompletionModel`](crate::paid) decorator instead
//! performs a fixed-price direct hold around each completion. Those paths must
//! never be nested.
//!
//! In both cases `run` stays a plain delegation to rig's `Prompt` trait.

use std::sync::Arc;

use aitp_client::PaymentGateway;
use rig_core::agent::Agent;
use rig_core::completion::CompletionModel;
use rig_core::completion::{Message, Prompt, PromptError};
use rig_core::streaming::StreamingPrompt;

use crate::paid::PricedRoute;
use crate::runtime::{build_agent, build_paid_agent, RuntimeConfig};

/// A ready-to-run agent. Payment (if any) is already composed into its model.
pub struct RuntimeAgent<M>
where
    M: CompletionModel,
{
    agent: Agent<M>,
}

impl<M> RuntimeAgent<M>
where
    M: CompletionModel + 'static,
{
    /// Build a plain agent over any completion model (no payment decoration).
    pub fn new(model: M, config: &RuntimeConfig) -> Self {
        Self {
            agent: build_agent(model, config),
        }
    }

    /// Wrap an already-built rig [`Agent`].
    pub fn from_agent(agent: Agent<M>) -> Self {
        Self { agent }
    }

    /// Run one prompt through the agent and return the accepted assistant text.
    ///
    /// Payment is automatic when the supplied model owns an AITP-aware HTTP
    /// transport, or when this agent was built with [`RuntimeAgent::paid`].
    pub async fn run(&self, prompt: impl Into<Message> + Send) -> Result<String, PromptError> {
        self.agent.prompt(prompt).await
    }

    /// Stream a high-level prompt through Rig's full agent loop.
    ///
    /// For `RelayModel::connect_paid`, the underlying HTTP stream performs the
    /// same policy-bound one-shot AITP handshake as `run` and will not report a
    /// clean terminal result without Relay's durable settlement evidence.
    pub async fn stream(
        &self,
        prompt: impl Into<Message> + Send,
    ) -> rig_core::agent::StreamingResult<M::StreamingResponse>
    where
        M::StreamingResponse:
            rig_core::completion::GetTokenUsage + rig_core::wasm_compat::WasmCompatSend,
    {
        self.agent.stream_prompt(prompt).await
    }

    /// Borrow the underlying rig [`Agent`] (for tool-loop / streaming APIs not
    /// surfaced by `run`).
    pub fn agent(&self) -> &Agent<M> {
        &self.agent
    }
}

impl<M, P> RuntimeAgent<crate::paid::PaidCompletionModel<M, P>>
where
    M: CompletionModel + 'static,
    P: PaymentGateway + 'static,
{
    /// Build an agent whose model is wrapped in the fixed-price direct-payment
    /// compatibility seam.
    ///
    /// Do not pass a Relay model that already handles Relay-issued AITP 402s;
    /// that would introduce two independent settlement owners.
    pub fn paid(model: M, gateway: Arc<P>, route: PricedRoute, config: &RuntimeConfig) -> Self {
        Self {
            agent: build_paid_agent(model, gateway, route, config),
        }
    }
}
