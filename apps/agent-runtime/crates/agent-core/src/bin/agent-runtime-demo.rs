//! `agent-runtime-demo` — a runnable, network-free call site for the direct-Mint
//! paid path.
//!
//! The audit noted that nothing in the repo actually *runs* a [`RuntimeAgent`]
//! down the paid path. This binary does: it builds a paid agent over the
//! in-process fake Mint ([`StubPaymentGateway`]) and rig's mock completion model,
//! prompts it, and prints the settlement calls that fired. It then exercises the
//! new capture reconciler by pointing a paid agent at a gateway whose `capture`
//! is flaky, showing the revenue is recovered (never silently lost).
//!
//! Run it with the `demo` feature (which pulls rig's `test-utils`):
//!
//! ```bash
//! cargo run -p agent-core --features demo --bin agent-runtime-demo
//! ```
//!
//! This binary is deliberately network-free. The same `RuntimeAgent` against a
//! LIVE Mint (`MintPaymentGateway`) and a live Relay model is the `live_agent`
//! example: `cargo run -p agent-core --example live_agent` (see its header for
//! the required `MINT_*` / `RELAY_*` / agent-key environment).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use agent_core::aitp_client::{
    AmountMicros, Authorized, HoldRef, PayError, PaymentChallenge, PaymentGateway, Receipt,
    StubPaymentGateway,
};
use agent_core::{
    build_agent, PaidCompletionModel, PricedRoute, RuntimeAgent, RuntimeConfig,
};
use rig_core::test_utils::MockCompletionModel;

fn route() -> PricedRoute {
    PricedRoute::new(
        "ait1qprovider",
        AmountMicros(4200),
        "POST /v1/chat/completions#demo-model",
    )
    .expect("valid priced route")
}

/// A gateway whose `capture` fails its first `fail_first` calls, then succeeds —
/// stands in for a Mint that momentarily drops capture responses.
struct FlakyCaptureGateway {
    inner: StubPaymentGateway,
    fail_first: usize,
    capture_calls: AtomicUsize,
}

impl FlakyCaptureGateway {
    fn new(fail_first: usize) -> Self {
        Self {
            inner: StubPaymentGateway::default(),
            fail_first,
            capture_calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait::async_trait]
impl PaymentGateway for FlakyCaptureGateway {
    async fn authorize(&self, challenge: &PaymentChallenge) -> Result<Authorized, PayError> {
        self.inner.authorize(challenge).await
    }
    async fn capture(&self, hold: &HoldRef, actual: AmountMicros) -> Result<Receipt, PayError> {
        let n = self.capture_calls.fetch_add(1, Ordering::SeqCst);
        if n < self.fail_first {
            return Err(PayError::Gateway(format!("capture attempt {n} lost")));
        }
        self.inner.capture(hold, actual).await
    }
    async fn release(&self, hold: &HoldRef) -> Result<(), PayError> {
        self.inner.release(hold).await
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    println!("== 1. paid path, happy case (StubPaymentGateway) ==");
    {
        let gateway = Arc::new(StubPaymentGateway::default());
        let agent = RuntimeAgent::paid(
            MockCompletionModel::text("hello from the paid agent"),
            gateway.clone(),
            route(),
            &RuntimeConfig {
                preamble: Some("demo agent".into()),
                ..Default::default()
            },
        );

        let reply = agent.run("say hi").await?;
        println!("  agent reply: {reply}");
        gateway.with_calls(|c| {
            println!(
                "  settlement: authorized={} captured={} released={}",
                c.authorized.len(),
                c.captured.len(),
                c.released.len(),
            );
            assert_eq!(c.captured.len(), 1, "happy path must capture exactly once");
            assert!(c.released.is_empty(), "happy path must not release");
        });
    }

    println!("\n== 2. paid path, capture is flaky (durable reconciliation) ==");
    {
        // Build the paid model directly so we can hold the reconciler handle
        // before the model is moved into the agent.
        let gateway = Arc::new(FlakyCaptureGateway::new(2)); // first 2 captures fail
        let paid_model = PaidCompletionModel::new(
            MockCompletionModel::text("served, but capture will stumble"),
            gateway.clone(),
            route(),
        );
        let reconciler = paid_model.reconciler();
        let agent = RuntimeAgent::from_agent(build_agent(paid_model, &RuntimeConfig::default()));

        // The first (inline) capture fails, so run() surfaces an error even
        // though inference was served — fail-closed, no free inference.
        let result = agent.run("please answer").await;
        println!("  inline run() result (expected Err): {result:?}");
        assert!(result.is_err(), "a lost capture must fail the request closed");

        // The reconciler now owns the debt. Drive it to settlement.
        for _ in 0..2_000 {
            if reconciler.pending_len() == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        // Belt and suspenders: an explicit shutdown-style drain (a no-op here
        // since the background loop already settled it).
        let settled_now = reconciler.reconcile_now().await.unwrap_or(0);

        println!(
            "  after reconciliation: capture_calls={} pending={} dead_letters={} (explicit drain settled {})",
            gateway.capture_calls.load(Ordering::SeqCst),
            reconciler.pending_len(),
            reconciler.dead_letter_len(),
            settled_now,
        );
        assert_eq!(reconciler.pending_len(), 0, "capture must be reconciled");
        assert_eq!(reconciler.dead_letter_len(), 0, "nothing dead-lettered");
        gateway
            .inner
            .with_calls(|c| assert_eq!(c.captured.len(), 1, "exactly one settled capture"));
        println!("  revenue recovered: the provider was served AND paid.");
    }

    println!("\ndemo complete.");
    Ok(())
}
