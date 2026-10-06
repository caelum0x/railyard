#![allow(dead_code)] // each test binary uses part of the harness

//! Shared harness for Ledgerline integration tests.
//!
//! Integration tests compile against the crate's public API only, so this in-
//! process `FakeMint` (an [`MintOps`] implementation) lives here rather than
//! reusing the crate-internal `#[cfg(test)]` double. It tracks integer-
//! microdollar balances, honors per-operation idempotency keys, and can be told
//! to fail specific primitives to inject mid-saga faults — the same technique the
//! unit tests use, but here driving the REAL Postgres outbox and store.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use ledgerline::domain::{EventOutcome, EventSource, MeterEvent};
use ledgerline::error::LedgerlineError;
use ledgerline::mint::{HoldRef, MintOps};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Op {
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
pub struct FakeMint {
    state: Mutex<FakeState>,
    /// When set, the next `create_hold` call waits on it before touching Mint
    /// state (a driver stalled mid-call); later calls pass straight through.
    hold_gate: Mutex<Option<Arc<tokio::sync::Notify>>>,
    /// Set once the gated call is waiting.
    gated_waiting: std::sync::atomic::AtomicBool,
}

impl FakeMint {
    pub fn fail(ops: &[Op]) -> Arc<Self> {
        let mint = Arc::new(Self::default());
        mint.state.lock().expect("lock").fail = ops.iter().copied().collect();
        mint
    }
    /// Stall the next `create_hold` until the returned notify fires.
    pub fn gate_next_hold(&self) -> Arc<tokio::sync::Notify> {
        let gate = Arc::new(tokio::sync::Notify::new());
        *self.hold_gate.lock().expect("lock") = Some(gate.clone());
        gate
    }
    /// Whether the gated `create_hold` call has started waiting.
    pub fn gated_hold_waiting(&self) -> bool {
        self.gated_waiting.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub fn clear_failures(&self) {
        self.state.lock().expect("lock").fail.clear();
    }
    pub fn stop_failing(&self, op: Op) {
        self.state.lock().expect("lock").fail.remove(&op);
    }
    pub fn balance(&self, account: &str) -> i64 {
        self.state
            .lock()
            .expect("lock")
            .balances
            .get(account)
            .copied()
            .unwrap_or(0)
    }
    pub fn effective_calls(&self, op: Op) -> u32 {
        self.state
            .lock()
            .expect("lock")
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
        let gate = self.hold_gate.lock().expect("lock").take();
        if let Some(gate) = gate {
            self.gated_waiting
                .store(true, std::sync::atomic::Ordering::SeqCst);
            gate.notified().await;
        }
        let mut state = self.state.lock().expect("lock");
        if let Some(prior) = state.applied.get(idem_key) {
            return Ok(HoldRef {
                hold_id: prior.hold_id.clone().expect("hold id cached"),
                first_seq: prior.seq.expect("seq cached"),
            });
        }
        if FakeMint::should_fail(&state, Op::CreateHold, idem_key) {
            return Err(LedgerlineError::Mint("injected create_hold failure".into()));
        }
        state.hold_counter += 1;
        state.seq += 1;
        let hold_id = format!("hold_{}", state.hold_counter);
        let seq = state.seq;
        state.credit(&event.customer_account, -event.revenue_micros);
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
        let mut state = self.state.lock().expect("lock");
        if let Some(prior) = state.applied.get(idem_key) {
            return Ok(prior.seq.expect("seq cached"));
        }
        if FakeMint::should_fail(&state, Op::CaptureHold, idem_key) {
            return Err(LedgerlineError::Mint("injected capture failure".into()));
        }
        state.seq += 1;
        let seq = state.seq;
        state.credit(&event.revenue_account, event.revenue_micros);
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
        let mut state = self.state.lock().expect("lock");
        if let Some(prior) = state.applied.get(idem_key) {
            return Ok(prior.seq.expect("seq cached"));
        }
        if FakeMint::should_fail(&state, Op::TransferCogs, idem_key) {
            return Err(LedgerlineError::Mint("injected cogs failure".into()));
        }
        state.seq += 1;
        let seq = state.seq;
        state.credit(&event.cogs_account, -event.cogs_micros);
        state.credit(&event.provider_account, event.cogs_micros);
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
        let mut state = self.state.lock().expect("lock");
        if state.applied.contains_key(idem_key) {
            return Ok(());
        }
        if FakeMint::should_fail(&state, Op::ReleaseHold, idem_key) {
            return Err(LedgerlineError::Mint("injected release failure".into()));
        }
        // Real Mint semantics: a distinct close of a terminal (captured/released)
        // hold is HoldNotFound, not a silent no-op.
        if state.captured_holds.contains(hold_id) || state.released_holds.contains(hold_id) {
            return Err(LedgerlineError::Mint(
                "HoldNotFound: cannot release a terminal (captured or released) hold".into(),
            ));
        }
        state.credit(&event.customer_account, event.revenue_micros);
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
        let mut state = self.state.lock().expect("lock");
        if state.applied.contains_key(idem_key) {
            return Ok(());
        }
        if FakeMint::should_fail(&state, Op::ReverseRevenue, idem_key) {
            return Err(LedgerlineError::Mint(
                "injected reverse_revenue failure".into(),
            ));
        }
        state.credit(&event.revenue_account, -event.revenue_micros);
        state.credit(&event.customer_account, event.revenue_micros);
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
        let mut state = self.state.lock().expect("lock");
        if state.applied.contains_key(idem_key) {
            return Ok(());
        }
        if FakeMint::should_fail(&state, Op::ReverseCogs, idem_key) {
            return Err(LedgerlineError::Mint(
                "injected reverse_cogs failure".into(),
            ));
        }
        state.credit(&event.provider_account, -event.cogs_micros);
        state.credit(&event.cogs_account, event.cogs_micros);
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

/// A successful event scoped to a unique `request_id`, so parallel tests and
/// repeated runs against a shared Postgres never collide.
pub fn event_with_request_id(request_id: &str) -> MeterEvent {
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

/// A process- and time-unique tag, so each test's request ids are disjoint.
pub fn unique_tag(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    )
}

pub fn assert_net_zero(mint: &FakeMint, event: &MeterEvent) {
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

pub fn assert_settled_once(mint: &FakeMint, event: &MeterEvent) {
    assert_eq!(mint.balance(&event.customer_account), -event.revenue_micros);
    assert_eq!(mint.balance(&event.revenue_account), event.revenue_micros);
    assert_eq!(mint.balance(&event.cogs_account), -event.cogs_micros);
    assert_eq!(mint.balance(&event.provider_account), event.cogs_micros);
}
