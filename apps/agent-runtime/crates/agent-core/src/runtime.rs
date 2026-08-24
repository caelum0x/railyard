//! The base runtime: turn a [`CompletionModel`] (optionally payment-decorated)
//! into a rig [`Agent`]. Turnstile and Abacus build on this.

use std::sync::Arc;

use aitp_client::PaymentGateway;
use rig_agent::{Agent, AgentBuilder};
use rig_core::completion::CompletionModel;

use crate::paid::{PaidCompletionModel, PricedRoute};

/// Minimal agent configuration shared by the economy apps.
#[derive(Debug, Clone, Default)]
pub struct RuntimeConfig {
    /// System preamble / instructions.
    pub preamble: Option<String>,
    /// Sampling temperature.
    pub temperature: Option<f64>,
    /// Response token cap.
    pub max_tokens: Option<u64>,
}

impl RuntimeConfig {
    fn apply(&self, mut builder: AgentBuilder) -> AgentBuilder {
        if let Some(preamble) = &self.preamble {
            builder = builder.preamble(preamble);
        }
        if let Some(t) = self.temperature {
            builder = builder.temperature(t);
        }
        if let Some(m) = self.max_tokens {
            builder = builder.max_tokens(m);
        }
        builder
    }
}

/// Build a plain agent over any completion model (no payment decoration).
pub fn build_agent<M>(model: M, config: &RuntimeConfig) -> Agent
where
    M: CompletionModel + 'static,
{
    config.apply(AgentBuilder::new(model)).build()
}

/// Build an agent whose model is wrapped in the AITP-402 payment seam.
pub fn build_paid_agent<M, P>(
    model: M,
    gateway: Arc<P>,
    route: PricedRoute,
    config: &RuntimeConfig,
) -> Agent
where
    M: CompletionModel + 'static,
    P: PaymentGateway + 'static,
{
    let paid = PaidCompletionModel::new(model, gateway, route);
    build_agent(paid, config)
}
