//! [`RuntimeAgent`] — a thin, ergonomic wrapper over a rig [`Agent`] that
//! exposes a single `run(prompt)` entry point for the economy apps
//! (Turnstile / Abacus).
//!
//! The AITP-402 hold is settled *around the inference* transparently: when the
//! agent is built with [`build_paid_agent`](crate::runtime::build_paid_agent),
//! its underlying model is the [`PaidCompletionModel`](crate::paid) decorator,
//! so every `run` authorizes a hold, performs the completion, then captures on
//! success or releases on failure — the caller just sees a `String` or an error.
//!
//! Keeping payment at the model layer (rather than a bespoke wrapper method)
//! means multi-turn tool loops inside rig's runtime each settle their own hold,
//! and `run` stays a plain delegation to rig's `Prompt` trait.

use std::sync::Arc;

use aitp_client::PaymentGateway;
use rig_agent::completion::{Message, Prompt, PromptError};
use rig_agent::Agent;
use rig_core::completion::CompletionModel;

use crate::paid::PricedRoute;
use crate::runtime::{build_agent, build_paid_agent, RuntimeConfig};

/// A ready-to-run agent. Wraps a model-erased rig [`Agent`]; payment (if any)
/// is already composed into the agent's model.
pub struct RuntimeAgent {
    agent: Agent,
}

impl RuntimeAgent {
    /// Build a plain agent over any completion model (no payment decoration).
    pub fn new<M>(model: M, config: &RuntimeConfig) -> Self
    where
        M: CompletionModel + 'static,
    {
        Self {
            agent: build_agent(model, config),
        }
    }

    /// Build an agent whose model is wrapped in the AITP-402 payment seam. Every
    /// [`run`](Self::run) then settles a hold around the inference.
    pub fn paid<M, P>(model: M, gateway: Arc<P>, route: PricedRoute, config: &RuntimeConfig) -> Self
    where
        M: CompletionModel + 'static,
        P: PaymentGateway + 'static,
    {
        Self {
            agent: build_paid_agent(model, gateway, route, config),
        }
    }

    /// Wrap an already-built rig [`Agent`].
    pub fn from_agent(agent: Agent) -> Self {
        Self { agent }
    }

    /// Run one prompt through the agent and return the accepted assistant text.
    ///
    /// If this agent was built with [`RuntimeAgent::paid`], the AITP-402 hold is
    /// authorized before and captured/released after the inference automatically.
    pub async fn run(&self, prompt: impl Into<Message> + Send) -> Result<String, PromptError> {
        self.agent.prompt(prompt).await
    }

    /// Borrow the underlying rig [`Agent`] (for tool-loop / streaming APIs not
    /// surfaced by `run`).
    pub fn agent(&self) -> &Agent {
        &self.agent
    }
}
