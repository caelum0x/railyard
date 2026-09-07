//! Driver: exercise ledgerline's REAL Postgres saga outbox through the
//! COMPENSATION path.
//!
//! The unit tests in `settle.rs` prove compensation only against `MemoryOutbox`.
//! This binary stands up the production `PostgresOutbox` (and `PostgresStore`
//! migrations) against a real Postgres and drives the durable settlement
//! coordinator through every compensation branch, asserting the money-correctness
//! invariants hold when the saga state is round-tripped through Postgres.
//!
//! Run with a Postgres reachable at `DATABASE_URL` (defaults to the docker
//! instance this task spins up on port 5435):
//!
//! ```text
//! cd apps/ledgerline && \
//!   DATABASE_URL=postgres://ledgerline:ledgerline@localhost:5435/ledgerline \
//!   cargo run --bin pg_saga_compensation
//! ```
//!
//! The Mint is an in-process `MintOps` that tracks integer-microdollar balances
//! and can be told to fail specific primitives to inject mid-saga faults — the
//! same technique the crate's unit tests use, but here the *outbox* is real PG.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ledgerline::domain::{EventOutcome, EventSource, MeterEvent};
use ledgerline::error::LedgerlineError;
use ledgerline::mint::{HoldRef, MintOps, MintSettlement};
use ledgerline::outbox::{OutboxStore, PostgresOutbox, SagaState, StepState};
use ledgerline::settle::{SettlementCoordinator, SystemClock};
use ledgerline::store::PostgresStore;

// ----------------------------------------------------------------------------
// In-process Mint that honors idempotency keys and tracks balances.
// ----------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Op {
    CreateHold,
    CaptureHold,
    TransferCogs,
    ReleaseHold,
    ReverseRevenue,
    ReverseCogs,
}

#[derive(Clone)]
struct Applied {
    hold_id: Option<String>,
    seq: Option<i64>,
}

#[derive(Default)]
struct FakeState {
    balances: HashMap<String, i64>,
    captured_holds: HashSet<String>,
    released_holds: HashSet<String>,
    applied: HashMap<String, Applied>,
    seq: i64,
    hold_counter: u64,
    fail: HashSet<Op>,
    effective_calls: HashMap<Op, u32>,
}

impl FakeState {
    fn credit(&mut self, account: &str, amount: i64) {
        *self.balances.entry(account.to_string()).or_insert(0) += amount;
    }
    fn record(&mut self, op: Op) {
        *self.effective_calls.entry(op).or_insert(0) += 1;
    }
}

#[derive(Default)]
struct FakeMint {
    state: Mutex<FakeState>,
}

impl FakeMint {
    fn fail(ops: &[Op]) -> Arc<Self> {
        let mint = Arc::new(Self::default());
        mint.state.lock().unwrap().fail = ops.iter().copied().collect();
        mint
    }
    fn clear_failures(&self) {
        self.state.lock().unwrap().fail.clear();
    }
    fn stop_failing(&self, op: Op) {
        self.state.lock().unwrap().fail.remove(&op);
    }
    fn balance(&self, account: &str) -> i64 {
        self.state
            .lock()
            .unwrap()
            .balances
            .get(account)
            .copied()
            .unwrap_or(0)
    }
    fn effective_calls(&self, op: Op) -> u32 {
        self.state
            .lock()
            .unwrap()
            .effective_calls
            .get(&op)
            .copied()
            .unwrap_or(0)
    }
    fn should_fail(state: &FakeState, op: Op, idem_key: &str) -> bool {
        !state.applied.contains_key(idem_key) && state.fail.contains(&op)
    }
}

#[async_trait]
impl MintOps for FakeMint {
    async fn create_hold(
        &self,
        event: &MeterEvent,
        idem_key: &str,
    ) -> Result<HoldRef, LedgerlineError> {
        let mut state = self.state.lock().unwrap();
        if let Some(prior) = state.applied.get(idem_key) {
            return Ok(HoldRef {
                hold_id: prior.hold_id.clone().unwrap(),
                first_seq: prior.seq.unwrap(),
            });
        }
        if FakeMint::should_fail(&state, Op::CreateHold, idem_key) {
            return Err(LedgerlineError::Mint("injected create_hold failure".into()));
        }
        state.hold_counter += 1;
        state.seq += 1;
        let hold_id = format!("hold_{}", state.hold_counter);
        let seq = state.seq;
        let revenue = event.revenue_micros;
        state.credit(&event.customer_account, -revenue);
        state.applied.insert(
            idem_key.to_string(),
            Applied {
                hold_id: Some(hold_id.clone()),
                seq: Some(seq),
            },
        );
        state.record(Op::CreateHold);
        Ok(HoldRef {
            hold_id,
            first_seq: seq,
        })
    }

    async fn capture_hold(
        &self,
        hold_id: &str,
        event: &MeterEvent,
        idem_key: &str,
    ) -> Result<i64, LedgerlineError> {
        let mut state = self.state.lock().unwrap();
        if let Some(prior) = state.applied.get(idem_key) {
            return Ok(prior.seq.unwrap());
        }
        if FakeMint::should_fail(&state, Op::CaptureHold, idem_key) {
            return Err(LedgerlineError::Mint("injected capture failure".into()));
        }
        state.seq += 1;
        let seq = state.seq;
        let revenue = event.revenue_micros;
        state.credit(&event.revenue_account, revenue);
        state.captured_holds.insert(hold_id.to_string());
        state.applied.insert(
            idem_key.to_string(),
            Applied {
                hold_id: None,
                seq: Some(seq),
            },
        );
        state.record(Op::CaptureHold);
        Ok(seq)
    }

    async fn transfer_cogs(
        &self,
        event: &MeterEvent,
        idem_key: &str,
    ) -> Result<i64, LedgerlineError> {
        let mut state = self.state.lock().unwrap();
        if let Some(prior) = state.applied.get(idem_key) {
            return Ok(prior.seq.unwrap());
        }
        if FakeMint::should_fail(&state, Op::TransferCogs, idem_key) {
            return Err(LedgerlineError::Mint("injected cogs failure".into()));
        }
        state.seq += 1;
        let seq = state.seq;
        let cogs = event.cogs_micros;
        state.credit(&event.cogs_account, -cogs);
        state.credit(&event.provider_account, cogs);
        state.applied.insert(
            idem_key.to_string(),
            Applied {
                hold_id: None,
                seq: Some(seq),
            },
        );
        state.record(Op::TransferCogs);
        Ok(seq)
    }

    async fn release_hold(
        &self,
        hold_id: &str,
        event: &MeterEvent,
        idem_key: &str,
    ) -> Result<(), LedgerlineError> {
        let mut state = self.state.lock().unwrap();
        if state.applied.contains_key(idem_key) {
            return Ok(());
        }
        if FakeMint::should_fail(&state, Op::ReleaseHold, idem_key) {
            return Err(LedgerlineError::Mint("injected release failure".into()));
        }
        // Real Mint semantics: a distinct close of a terminal (captured/released)
        // hold is HoldNotFound, not a no-op.
        if state.captured_holds.contains(hold_id) || state.released_holds.contains(hold_id) {
            return Err(LedgerlineError::Mint(
                "HoldNotFound: cannot release a terminal (captured or released) hold".into(),
            ));
        }
        let revenue = event.revenue_micros;
        state.credit(&event.customer_account, revenue);
        state.released_holds.insert(hold_id.to_string());
        state.applied.insert(
            idem_key.to_string(),
            Applied {
                hold_id: None,
                seq: None,
            },
        );
        state.record(Op::ReleaseHold);
        Ok(())
    }

    async fn reverse_revenue(
        &self,
        event: &MeterEvent,
        idem_key: &str,
    ) -> Result<(), LedgerlineError> {
        let mut state = self.state.lock().unwrap();
        if state.applied.contains_key(idem_key) {
            return Ok(());
        }
        if FakeMint::should_fail(&state, Op::ReverseRevenue, idem_key) {
            return Err(LedgerlineError::Mint(
                "injected reverse_revenue failure".into(),
            ));
        }
        let revenue = event.revenue_micros;
        state.credit(&event.revenue_account, -revenue);
        state.credit(&event.customer_account, revenue);
        state.applied.insert(
            idem_key.to_string(),
            Applied {
                hold_id: None,
                seq: None,
            },
        );
        state.record(Op::ReverseRevenue);
        Ok(())
    }

    async fn reverse_cogs(
        &self,
        event: &MeterEvent,
        idem_key: &str,
    ) -> Result<(), LedgerlineError> {
        let mut state = self.state.lock().unwrap();
        if state.applied.contains_key(idem_key) {
            return Ok(());
        }
        if FakeMint::should_fail(&state, Op::ReverseCogs, idem_key) {
            return Err(LedgerlineError::Mint(
                "injected reverse_cogs failure".into(),
            ));
        }
        let cogs = event.cogs_micros;
        state.credit(&event.provider_account, -cogs);
        state.credit(&event.cogs_account, cogs);
        state.applied.insert(
            idem_key.to_string(),
            Applied {
                hold_id: None,
                seq: None,
            },
        );
        state.record(Op::ReverseCogs);
        Ok(())
    }
}

// ----------------------------------------------------------------------------
// Test harness helpers.
// ----------------------------------------------------------------------------

/// A unique request id per scenario so runs are independent and idempotent.
fn event_with_request_id(request_id: &str) -> MeterEvent {
    MeterEvent {
        idempotency_key: format!("idem-{request_id}"),
        request_id: request_id.to_string(),
        customer_id: "customer-1".into(),
        feature: "render".into(),
        model: "wan-2.2".into(),
        source: EventSource::Reelforge,
        outcome: EventOutcome::Succeeded,
        revenue_micros: 900_000,
        cogs_micros: 600_000,
        customer_account: format!("acct_customer:{request_id}"),
        revenue_account: format!("acct_revenue:{request_id}"),
        cogs_account: format!("acct_cogs:{request_id}"),
        provider_account: format!("acct_provider:{request_id}"),
        source_settlement_receipt: None,
    }
}

fn assert_net_zero(mint: &FakeMint, event: &MeterEvent) {
    for (label, account) in [
        ("customer", &event.customer_account),
        ("revenue", &event.revenue_account),
        ("cogs", &event.cogs_account),
        ("provider", &event.provider_account),
    ] {
        let balance = mint.balance(account);
        assert_eq!(
            balance, 0,
            "{label} account must net to zero, was {balance}"
        );
    }
}

fn assert_settled_once(mint: &FakeMint, event: &MeterEvent) {
    assert_eq!(mint.balance(&event.customer_account), -event.revenue_micros);
    assert_eq!(mint.balance(&event.revenue_account), event.revenue_micros);
    assert_eq!(mint.balance(&event.cogs_account), -event.cogs_micros);
    assert_eq!(mint.balance(&event.provider_account), event.cogs_micros);
}

macro_rules! check {
    ($label:expr, $cond:expr) => {{
        if $cond {
            println!("    PASS  {}", $label);
        } else {
            eprintln!("    FAIL  {}", $label);
            std::process::exit(1);
        }
    }};
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://ledgerline:ledgerline@localhost:5435/ledgerline".to_string()
    });
    let run_tag = format!(
        "{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    println!("== ledgerline PG saga compensation driver ==");
    println!("database_url = {database_url}");

    let store = PostgresStore::connect(&database_url).await?;
    store.migrate().await?;
    println!("migrations applied (0001 events, 0002 settlement_outbox, 0003 source receipts)");
    let outbox: Arc<dyn OutboxStore> = Arc::new(PostgresOutbox::new(store.pool().clone()));

    // ------------------------------------------------------------------
    // Scenario A: COGS fails after hold+capture commit -> full compensation
    // via reverse_revenue (captured hold is terminal, never released).
    // ------------------------------------------------------------------
    println!("\n[A] COGS transfer fails after capture -> compensate via reverse_revenue");
    let event_a = event_with_request_id(&format!("req-A-{run_tag}"));
    let mint_a = FakeMint::fail(&[Op::TransferCogs]);
    let coord_a = SettlementCoordinator::new(mint_a.clone(), outbox.clone(), Arc::new(SystemClock));
    let result_a = coord_a.settle(&event_a).await;
    check!("settle() surfaces the injected failure", result_a.is_err());
    assert_net_zero(&mint_a, &event_a);
    check!("all four accounts net to zero in the Mint ledger", true);
    check!(
        "reverse_revenue ran exactly once",
        mint_a.effective_calls(Op::ReverseRevenue) == 1
    );
    check!(
        "captured hold was NEVER released (terminal-hold correctness)",
        mint_a.effective_calls(Op::ReleaseHold) == 0
    );
    let saga_a = outbox.load(&event_a.request_id).await?.expect("PG saga A");
    check!(
        "PG saga.state == Compensated (did not wedge in Compensating)",
        saga_a.state == SagaState::Compensated
    );
    check!(
        "PG saga.hold_state == Compensated",
        saga_a.hold_state == StepState::Compensated
    );
    check!(
        "PG saga.capture_state == Compensated",
        saga_a.capture_state == StepState::Compensated
    );
    check!(
        "PG saga.cogs_state == Pending (never committed)",
        saga_a.cogs_state == StepState::Pending
    );

    // Retry after the fault clears must settle exactly once on a fresh attempt.
    println!("[A] fault clears -> retry settles once on attempt 2");
    mint_a.clear_failures();
    let refs_a = coord_a.settle(&event_a).await.expect("retry settles");
    check!(
        "retry returns a cogs_sequence",
        refs_a.cogs_sequence.is_some()
    );
    assert_settled_once(&mint_a, &event_a);
    check!("ledger shows exactly one settlement after retry", true);
    let saga_a2 = outbox.load(&event_a.request_id).await?.expect("PG saga A2");
    check!(
        "PG saga.state == Settled after retry",
        saga_a2.state == SagaState::Settled
    );
    check!("PG saga.attempt == 2 after retry", saga_a2.attempt == 2);
    check!(
        "transfer_cogs committed exactly once across both attempts",
        mint_a.effective_calls(Op::TransferCogs) == 1
    );

    // ------------------------------------------------------------------
    // Scenario B: capture fails -> only the hold is committed -> release it.
    // ------------------------------------------------------------------
    println!("\n[B] capture fails -> uncaptured hold is released to net zero");
    let event_b = event_with_request_id(&format!("req-B-{run_tag}"));
    let mint_b = FakeMint::fail(&[Op::CaptureHold]);
    let coord_b = SettlementCoordinator::new(mint_b.clone(), outbox.clone(), Arc::new(SystemClock));
    check!(
        "settle() surfaces the capture failure",
        coord_b.settle(&event_b).await.is_err()
    );
    assert_net_zero(&mint_b, &event_b);
    check!("accounts net to zero", true);
    check!(
        "release_hold ran exactly once (uncaptured hold)",
        mint_b.effective_calls(Op::ReleaseHold) == 1
    );
    check!(
        "reverse_revenue did NOT run (nothing captured)",
        mint_b.effective_calls(Op::ReverseRevenue) == 0
    );
    let saga_b = outbox.load(&event_b.request_id).await?.expect("PG saga B");
    check!(
        "PG saga.state == Compensated",
        saga_b.state == SagaState::Compensated
    );
    check!(
        "PG saga.hold_state == Compensated",
        saga_b.hold_state == StepState::Compensated
    );

    // ------------------------------------------------------------------
    // Scenario C: forward fails at COGS AND the first reversal (reverse_revenue)
    // also fails -> saga wedges durably `Compensating` in PG; a later call
    // (fault cleared) must resume from the PG row and reach `Compensated`.
    // ------------------------------------------------------------------
    println!("\n[C] mid-compensation crash persists as Compensating in PG, then resumes");
    let event_c = event_with_request_id(&format!("req-C-{run_tag}"));
    let mint_c = FakeMint::fail(&[Op::TransferCogs, Op::ReverseRevenue]);
    let coord_c = SettlementCoordinator::new(mint_c.clone(), outbox.clone(), Arc::new(SystemClock));
    check!(
        "first settle() fails (forward + reversal both faulted)",
        coord_c.settle(&event_c).await.is_err()
    );
    let saga_c = outbox.load(&event_c.request_id).await?.expect("PG saga C");
    check!(
        "PG saga.state == Compensating (durably mid-reversal)",
        saga_c.state == SagaState::Compensating
    );
    check!(
        "PG saga.capture_state still Committed (reversal not yet done)",
        saga_c.capture_state == StepState::Committed
    );
    // The recovery query must see this stuck saga.
    let stuck = outbox
        .list_incomplete(chrono::Utc::now() + chrono::Duration::seconds(60), 100)
        .await?;
    check!(
        "list_incomplete() surfaces the Compensating saga from PG",
        stuck.iter().any(|s| s.request_id == event_c.request_id)
    );

    println!("[C] reversal fault clears -> resume finishes compensation");
    mint_c.stop_failing(Op::ReverseRevenue);
    check!(
        "resume settle() still reports failure (attempt did not settle)",
        coord_c.settle(&event_c).await.is_err()
    );
    assert_net_zero(&mint_c, &event_c);
    check!("accounts net to zero after resumed compensation", true);
    let saga_c2 = outbox.load(&event_c.request_id).await?.expect("PG saga C2");
    check!(
        "PG saga.state == Compensated after resume",
        saga_c2.state == SagaState::Compensated
    );

    println!("\nALL PG SAGA COMPENSATION INVARIANTS HELD.");
    Ok(())
}
