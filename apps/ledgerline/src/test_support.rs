//! Shared, crate-internal test doubles for the settlement path.
//!
//! This is the single faithful in-memory Mint used by both the coordinator tests
//! ([`crate::settle`]) and the recovery-worker tests ([`crate::recovery`]). It is
//! compiled only under `cfg(test)`. Keeping one double (rather than a per-module
//! copy) means recovery tests exercise the exact same money semantics — balances,
//! idempotency keys, terminal-hold rules — that the coordinator tests rely on.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::domain::{EventOutcome, EventSource, MeterEvent};
use crate::error::LedgerlineError;
use crate::mint::{HoldRef, MintOps};
use crate::outbox::MemoryOutbox;
use crate::settle::{Clock, SettlementCoordinator};

/// Fixed clock for deterministic saga timestamps.
struct FixedClock(DateTime<Utc>);
impl Clock for FixedClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

/// A single, fixed instant used by every settlement test.
pub fn fixed_clock() -> Arc<dyn Clock> {
    Arc::new(FixedClock(
        DateTime::parse_from_rfc3339("2026-08-25T00:00:00Z")
            .expect("valid timestamp")
            .with_timezone(&Utc),
    ))
}

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

/// In-memory Mint that tracks balances, honors idempotency keys, and can be
/// told to fail specific operations to inject mid-sequence faults.
#[derive(Default)]
pub struct FakeMint {
    state: Mutex<FakeState>,
}

impl FakeMint {
    pub fn fail(ops: &[Op]) -> Arc<Self> {
        let mint = Arc::new(Self::default());
        {
            let mut state = mint.state.lock().expect("lock");
            state.fail = ops.iter().copied().collect();
        }
        mint
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
        // Idempotent replay of *this* release (same idem_key) is a no-op,
        // exactly like the real Mint returning the cached outcome.
        if state.applied.contains_key(idem_key) {
            return Ok(());
        }
        if FakeMint::should_fail(&state, Op::ReleaseHold, idem_key) {
            return Err(LedgerlineError::Mint("injected release failure".into()));
        }
        // Real Mint semantics (mint-ledger release_hold / mint-store
        // commit_hold_release): a hold is Open only until it reaches a
        // terminal state. A *distinct* close (fresh idem_key) of a hold that
        // was ever captured — or already released — is `HoldNotFound`, not a
        // silent no-op. Ledgerline has no dedicated HoldNotFound variant, so
        // the real client surfaces this as `LedgerlineError::Mint`.
        if state.captured_holds.contains(hold_id) || state.released_holds.contains(hold_id) {
            return Err(LedgerlineError::Mint(
                "HoldNotFound: cannot release a terminal (captured or released) hold".into(),
            ));
        }
        // Open hold: free the escrow back to the customer and close it.
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

/// A canonical successful event: revenue 900k micros, COGS 600k micros.
pub fn event() -> MeterEvent {
    MeterEvent {
        idempotency_key: "idem-1".into(),
        request_id: "request-1".into(),
        customer_id: "customer-1".into(),
        feature: "render".into(),
        model: "wan-2.2".into(),
        source: EventSource::Reelforge,
        outcome: EventOutcome::Succeeded,
        revenue_micros: 900_000,
        cogs_micros: 600_000,
        customer_account: "acct_customer".into(),
        revenue_account: "acct_revenue".into(),
        cogs_account: "acct_cogs".into(),
        provider_account: "acct_provider".into(),
        source_settlement_receipt: None,
    }
}

/// Build a coordinator over the shared fake Mint, outbox, and fixed clock.
pub fn coordinator(mint: Arc<FakeMint>, outbox: Arc<MemoryOutbox>) -> SettlementCoordinator {
    SettlementCoordinator::new(mint, outbox, fixed_clock())
}

/// Assert every account is back to its zero starting balance (net-zero money).
pub fn assert_fully_compensated(mint: &FakeMint, event: &MeterEvent) {
    assert_eq!(
        mint.balance(&event.customer_account),
        0,
        "customer net zero"
    );
    assert_eq!(mint.balance(&event.revenue_account), 0, "revenue net zero");
    assert_eq!(mint.balance(&event.cogs_account), 0, "cogs net zero");
    assert_eq!(
        mint.balance(&event.provider_account),
        0,
        "provider net zero"
    );
}

/// Assert the ledger reflects exactly one full settlement of `event`.
pub fn assert_settled_once(mint: &FakeMint, event: &MeterEvent) {
    assert_eq!(
        mint.balance(&event.customer_account),
        -event.revenue_micros,
        "customer charged exactly once"
    );
    assert_eq!(
        mint.balance(&event.revenue_account),
        event.revenue_micros,
        "revenue credited exactly once"
    );
    assert_eq!(
        mint.balance(&event.cogs_account),
        -event.cogs_micros,
        "cogs debited exactly once"
    );
    assert_eq!(
        mint.balance(&event.provider_account),
        event.cogs_micros,
        "provider paid exactly once"
    );
}
