//! Minimal example agent for the agent-economy apps (Turnstile / Abacus).
//!
//! Run it offline (no LLM, no network) with:
//!
//! ```text
//! cd apps/agent-runtime && cargo run -p agent-core --example minimal_agent
//! ```
//!
//! It demonstrates the whole base runtime end to end:
//!   1. build a completion model,
//!   2. wrap it in the AITP-402 payment seam ([`StubPaymentGateway`] for now),
//!   3. build a [`RuntimeAgent`], and
//!   4. `run` a prompt — a payment hold is authorized before the inference and
//!      captured after it, all behind the scenes.
//!
//! Here the model is rig's `MockCompletionModel` so the example needs no live
//! Relay. The commented block at the bottom shows the single line you swap in to
//! talk to a real Relay gateway instead — the rest of the code is identical.

use std::sync::Arc;

use agent_core::{PricedRoute, RuntimeAgent, RuntimeConfig};
use aitp_client::{AmountMicros, StubPaymentGateway};
use rig_core::test_utils::MockCompletionModel;

#[tokio::main]
async fn main() {
    // (1) A completion model. Offline here; see the note below for real Relay.
    let model = MockCompletionModel::text("Hi! I'm a paid agent running offline.");

    // (2) The AITP-402 payment seam. StubPaymentGateway authorizes deterministically
    //     and records every call. A Mint-backed gateway drops in later (AP3) with no
    //     change to this file.
    let gateway = Arc::new(StubPaymentGateway::default());

    // (3) The priced route this agent pays for. In production the challenge comes
    //     from Relay's HTTP 402; until Relay billing is on we price it locally.
    let route = PricedRoute::new(
        "ait1qprovider_receive_addr",
        AmountMicros(4_200), // µ$ — never floats
        "POST /openai/v1/chat/completions#tensorzero::model_name::openai::gpt-4o-mini",
    );

    let agent = RuntimeAgent::paid(
        model,
        gateway.clone(),
        route,
        &RuntimeConfig {
            preamble: Some("You are a concise, helpful economy agent.".into()),
            temperature: Some(0.2),
            ..Default::default()
        },
    );

    // (4) Run a prompt. The hold is authorized -> inference -> captured.
    let reply = agent
        .run("Say hello in one sentence.")
        .await
        .expect("agent run failed");

    println!("agent> {reply}");

    gateway.with_calls(|c| {
        println!(
            "aitp-402> authorized={} captured={} released={}",
            c.authorized.len(),
            c.captured.len(),
            c.released.len(),
        );
    });

    // --- Talking to a REAL Relay gateway ------------------------------------
    // Swap the mock for a live Relay-backed model. Everything else is unchanged:
    //
    //   use relay_provider::{RelayConfig, RelayModel};
    //   let cfg = RelayConfig::model_name(
    //       std::env::var("RELAY_URL").unwrap() + "/openai/v1",
    //       std::env::var("RELAY_API_KEY").unwrap(),
    //       "openai::gpt-4o-mini",
    //   );
    //   let model = RelayModel::connect(&cfg).expect("relay client");
    //   let agent = RuntimeAgent::paid(model, gateway, route, &config);
    //   let reply = agent.run("Say hello.").await?;
}
