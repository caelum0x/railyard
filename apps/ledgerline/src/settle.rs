//! Durable settlement coordinator: drives the hold+capture+COGS trio through the
//! outbox saga so it is atomic-or-compensated.
//!
//! The coordinator implements [`MintSettlement`], so the server calls it exactly
//! like the old direct client. Internally it:
//!
//! 1. loads (or creates) the request's [`SettlementSaga`] from the durable
//!    [`OutboxStore`], persisting intent *before* any Mint call;
//! 2. runs the three forward [`MintOps`] steps, recording each commit durably;
//! 3. on any step failure, reverses the already-committed steps with their
//!    compensations, so the attempt nets to zero — no half-settled state.
//!
//! Every Mint primitive carries a per-attempt idempotency key, so a crash-resume
//! re-runs the trio as no-ops up to the last committed step, and a retry after a
//! compensated attempt gets fresh keys and settles cleanly exactly once.

use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};

use crate::domain::{EventOutcome, MeterEvent, SettlementRefs};
use crate::error::LedgerlineError;
use crate::mint::{MintOps, MintSettlement};
use crate::outbox::{OutboxStore, SagaState, SettlementSaga, StepState};

/// Injected time source, so saga timestamps are deterministic under test.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// Wall-clock implementation used in production.
#[derive(Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Deterministic settlement coordinator over injectable Mint ops, a durable
/// outbox, and a clock.
#[derive(Clone)]
pub struct SettlementCoordinator {
    ops: Arc<dyn MintOps>,
    outbox: Arc<dyn OutboxStore>,
    clock: Arc<dyn Clock>,
}

impl SettlementCoordinator {
    pub fn new(ops: Arc<dyn MintOps>, outbox: Arc<dyn OutboxStore>, clock: Arc<dyn Clock>) -> Self {
        Self { ops, outbox, clock }
    }

    async fn run_forward(
        &self,
        saga: &mut SettlementSaga,
        event: &MeterEvent,
    ) -> Result<SettlementRefs, LedgerlineError> {
        if saga.hold_state != StepState::Committed {
            let key = step_key(&saga.request_id, saga.attempt, StepKey::Hold);
            match self.ops.create_hold(event, &key).await {
                Ok(hold) => {
                    saga.hold_id = Some(hold.hold_id);
                    saga.hold_seq = Some(hold.first_seq);
                    saga.hold_state = StepState::Committed;
                    saga.updated_at = self.clock.now();
                    self.outbox.upsert(saga).await?;
                }
                Err(cause) => return self.begin_compensation(saga, event, cause).await,
            }
        }

        if saga.capture_state != StepState::Committed {
            let hold_id = saga.hold_id.clone().ok_or_else(|| {
                LedgerlineError::Storage("capture step reached without a hold id".into())
            })?;
            let key = step_key(&saga.request_id, saga.attempt, StepKey::Capture);
            match self.ops.capture_hold(&hold_id, event, &key).await {
                Ok(seq) => {
                    saga.revenue_seq = Some(seq);
                    saga.capture_state = StepState::Committed;
                    saga.updated_at = self.clock.now();
                    self.outbox.upsert(saga).await?;
                }
                Err(cause) => return self.begin_compensation(saga, event, cause).await,
            }
        }

        if saga.cogs_state != StepState::Committed {
            let key = step_key(&saga.request_id, saga.attempt, StepKey::Cogs);
            match self.ops.transfer_cogs(event, &key).await {
                Ok(seq) => {
                    saga.cogs_seq = Some(seq);
                    saga.cogs_state = StepState::Committed;
                    saga.updated_at = self.clock.now();
                    self.outbox.upsert(saga).await?;
                }
                Err(cause) => return self.begin_compensation(saga, event, cause).await,
            }
        }

        saga.state = SagaState::Settled;
        saga.updated_at = self.clock.now();
        self.outbox.upsert(saga).await?;
        Ok(saga.refs())
    }

    /// Mark the saga as compensating, reverse committed steps, then surface the
    /// original failure. If a compensation itself fails, the saga is left durably
    /// `Compensating` and the error propagates so a later call can resume.
    async fn begin_compensation(
        &self,
        saga: &mut SettlementSaga,
        event: &MeterEvent,
        cause: LedgerlineError,
    ) -> Result<SettlementRefs, LedgerlineError> {
        saga.state = SagaState::Compensating;
        saga.updated_at = self.clock.now();
        self.outbox.upsert(saga).await?;
        self.run_compensation(saga, event).await?;
        Err(cause)
    }

    /// Reverse committed steps in strict reverse order. Each reversal is keyed and
    /// idempotent, and progress is persisted after each success, so this is safe
    /// to call repeatedly until the saga reaches `Compensated`.
    async fn run_compensation(
        &self,
        saga: &mut SettlementSaga,
        event: &MeterEvent,
    ) -> Result<(), LedgerlineError> {
        if saga.cogs_state == StepState::Committed {
            let key = step_key(&saga.request_id, saga.attempt, StepKey::CogsReverse);
            self.ops.reverse_cogs(event, &key).await?;
            saga.cogs_state = StepState::Compensated;
            saga.updated_at = self.clock.now();
            self.outbox.upsert(saga).await?;
        }

        if saga.capture_state == StepState::Committed {
            let key = step_key(&saga.request_id, saga.attempt, StepKey::RevenueReverse);
            self.ops.reverse_revenue(event, &key).await?;
            saga.capture_state = StepState::Compensated;
            saga.updated_at = self.clock.now();
            self.outbox.upsert(saga).await?;
        }

        if saga.hold_state == StepState::Committed {
            // A hold that was ever captured is terminal on Mint: capture consumed
            // the escrow, so `reverse_revenue` above (the reverse transfer) is the
            // whole compensation and already returned the funds to the customer.
            // Releasing such a hold is a *distinct* close of a terminal hold, which
            // Mint answers with `HoldNotFound` — so we must NOT attempt it. Only an
            // uncaptured hold (capture never committed) still holds escrow and must
            // be released to net to zero.
            let was_captured = saga.capture_state != StepState::Pending;
            if !was_captured {
                let hold_id = saga.hold_id.clone().ok_or_else(|| {
                    LedgerlineError::Storage("hold release reached without a hold id".into())
                })?;
                let key = step_key(&saga.request_id, saga.attempt, StepKey::HoldRelease);
                self.ops.release_hold(&hold_id, event, &key).await?;
            }
            saga.hold_state = StepState::Compensated;
            saga.updated_at = self.clock.now();
            self.outbox.upsert(saga).await?;
        }

        saga.state = SagaState::Compensated;
        saga.updated_at = self.clock.now();
        self.outbox.upsert(saga).await?;
        Ok(())
    }
}

#[async_trait]
impl MintSettlement for SettlementCoordinator {
    async fn settle(&self, event: &MeterEvent) -> Result<SettlementRefs, LedgerlineError> {
        // Failed requests never move money; there is nothing to settle.
        if event.outcome == EventOutcome::Failed {
            return Ok(SettlementRefs::failed_request());
        }

        let mut saga = match self.outbox.load(&event.request_id).await? {
            Some(existing) => match existing.state {
                // Already fully settled: idempotent replay, no new Mint calls.
                SagaState::Settled => return Ok(existing.refs()),
                // A prior attempt failed mid-reversal: finish it, then report.
                SagaState::Compensating => {
                    let mut resumed = existing;
                    self.run_compensation(&mut resumed, event).await?;
                    return Err(LedgerlineError::Mint(
                        "settlement was compensated after a prior failure".into(),
                    ));
                }
                // Prior attempt cleanly rolled back: retry on a fresh attempt.
                SagaState::Compensated => existing.next_attempt(self.clock.now()),
                // Prior attempt crashed mid-flight: resume the same attempt.
                SagaState::Pending => existing,
            },
            None => SettlementSaga::new(&event.request_id, self.clock.now()),
        };

        saga.state = SagaState::Pending;
        saga.updated_at = self.clock.now();
        self.outbox.upsert(&saga).await?;
        self.run_forward(&mut saga, event).await
    }
}

/// The distinct Mint operations a saga can key.
#[derive(Clone, Copy)]
pub(crate) enum StepKey {
    Hold,
    Capture,
    Cogs,
    CogsReverse,
    RevenueReverse,
    HoldRelease,
}

impl StepKey {
    const fn as_str(self) -> &'static str {
        match self {
            StepKey::Hold => "revenue:hold",
            StepKey::Capture => "revenue:capture",
            StepKey::Cogs => "cogs:transfer",
            StepKey::CogsReverse => "cogs:reverse",
            StepKey::RevenueReverse => "revenue:reverse",
            StepKey::HoldRelease => "hold:release",
        }
    }
}

/// Deterministic per-attempt idempotency key for one Mint operation.
///
/// `request_id` is the financial identity; `attempt` scopes a fresh retry after a
/// compensated attempt so reused keys can never make a fresh forward call collapse
/// into a no-op replay of an already-reversed operation.
pub(crate) fn step_key(request_id: &str, attempt: u32, op: StepKey) -> String {
    format!("ledgerline:{request_id}:a{attempt}:{}", op.as_str())
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;

    use super::*;
    use crate::domain::{EventSource, MeterEvent};
    use crate::mint::HoldRef;
    use crate::outbox::MemoryOutbox;

    /// Fixed clock for deterministic saga timestamps.
    struct FixedClock(DateTime<Utc>);
    impl Clock for FixedClock {
        fn now(&self) -> DateTime<Utc> {
            self.0
        }
    }

    fn fixed_clock() -> Arc<dyn Clock> {
        Arc::new(FixedClock(
            DateTime::parse_from_rfc3339("2026-08-25T00:00:00Z")
                .expect("valid timestamp")
                .with_timezone(&Utc),
        ))
    }

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

    /// In-memory Mint that tracks balances, honors idempotency keys, and can be
    /// told to fail specific operations to inject mid-sequence faults.
    #[derive(Default)]
    struct FakeMint {
        state: Mutex<FakeState>,
    }

    impl FakeMint {
        fn fail(ops: &[Op]) -> Arc<Self> {
            let mint = Arc::new(Self::default());
            {
                let mut state = mint.state.lock().expect("lock");
                state.fail = ops.iter().copied().collect();
            }
            mint
        }
        fn clear_failures(&self) {
            self.state.lock().expect("lock").fail.clear();
        }
        fn stop_failing(&self, op: Op) {
            self.state.lock().expect("lock").fail.remove(&op);
        }
        fn balance(&self, account: &str) -> i64 {
            self.state
                .lock()
                .expect("lock")
                .balances
                .get(account)
                .copied()
                .unwrap_or(0)
        }
        fn effective_calls(&self, op: Op) -> u32 {
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

    fn event() -> MeterEvent {
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
        }
    }

    fn coordinator(mint: Arc<FakeMint>, outbox: Arc<MemoryOutbox>) -> SettlementCoordinator {
        SettlementCoordinator::new(mint, outbox, fixed_clock())
    }

    /// Assert every account is back to its zero starting balance.
    fn assert_fully_compensated(mint: &FakeMint, event: &MeterEvent) {
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
    fn assert_settled_once(mint: &FakeMint, event: &MeterEvent) {
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

    #[tokio::test]
    async fn happy_path_settles_whole_trio() {
        let mint = Arc::new(FakeMint::default());
        let outbox = Arc::new(MemoryOutbox::new());
        let coordinator = coordinator(mint.clone(), outbox.clone());
        let event = event();

        let refs = coordinator.settle(&event).await.expect("settle");
        assert!(refs.revenue_hold_id.is_some());
        assert!(refs.revenue_sequence.is_some());
        assert!(refs.cogs_sequence.is_some());
        assert_settled_once(&mint, &event);
        let saga = outbox
            .load(&event.request_id)
            .await
            .expect("load")
            .expect("saga");
        assert_eq!(saga.state, SagaState::Settled);
    }

    #[tokio::test]
    async fn failure_between_capture_and_cogs_fully_compensates() {
        // Inject a fault at the COGS transfer — after hold+capture have committed.
        let mint = FakeMint::fail(&[Op::TransferCogs]);
        let outbox = Arc::new(MemoryOutbox::new());
        let coordinator = coordinator(mint.clone(), outbox.clone());
        let event = event();

        let result = coordinator.settle(&event).await;
        assert!(result.is_err(), "settlement must surface the failure");

        // Invariant: never partial. Every account is back to zero.
        assert_fully_compensated(&mint, &event);
        let saga = outbox
            .load(&event.request_id)
            .await
            .expect("load")
            .expect("saga");
        assert_eq!(saga.state, SagaState::Compensated);
        assert_eq!(saga.hold_state, StepState::Compensated);
        assert_eq!(saga.capture_state, StepState::Compensated);
        assert_eq!(saga.cogs_state, StepState::Pending);
    }

    #[tokio::test]
    async fn captured_hold_compensates_via_reverse_revenue_not_release() {
        // The flagship scenario: hold + capture commit, then COGS fails. The
        // captured hold is terminal on Mint, so compensation MUST reverse the
        // revenue (the reverse transfer) and MUST NOT release the hold — releasing
        // a terminal hold is a distinct close that Mint answers with HoldNotFound.
        // The saga must reach `Compensated`, never wedge in `Compensating`.
        let mint = FakeMint::fail(&[Op::TransferCogs]);
        let outbox = Arc::new(MemoryOutbox::new());
        let coordinator = coordinator(mint.clone(), outbox.clone());
        let event = event();

        assert!(coordinator.settle(&event).await.is_err());

        // reverse_revenue ran; release_hold did NOT (captured hold is terminal).
        assert_eq!(
            mint.effective_calls(Op::ReverseRevenue),
            1,
            "reverse_revenue is the compensation for a captured hold"
        );
        assert_eq!(
            mint.effective_calls(Op::ReleaseHold),
            0,
            "a captured hold must never be released"
        );
        assert_fully_compensated(&mint, &event);

        let saga = outbox
            .load(&event.request_id)
            .await
            .expect("load")
            .expect("saga");
        assert_eq!(
            saga.state,
            SagaState::Compensated,
            "saga must fully compensate, not wedge in Compensating"
        );
        assert_eq!(saga.hold_state, StepState::Compensated);
        assert_eq!(saga.capture_state, StepState::Compensated);

        // Re-running after compensation converges (fresh attempt) and never wedges.
        mint.clear_failures();
        let refs = coordinator.settle(&event).await.expect("retry settles");
        assert!(refs.cogs_sequence.is_some());
        assert_settled_once(&mint, &event);
    }

    #[tokio::test]
    async fn retry_after_compensation_settles_exactly_once() {
        let mint = FakeMint::fail(&[Op::TransferCogs]);
        let outbox = Arc::new(MemoryOutbox::new());
        let coordinator = coordinator(mint.clone(), outbox.clone());
        let event = event();

        // First attempt fails at COGS and fully compensates.
        assert!(coordinator.settle(&event).await.is_err());
        assert_fully_compensated(&mint, &event);

        // The fault clears; a retry must settle cleanly, exactly once.
        mint.clear_failures();
        let refs = coordinator.settle(&event).await.expect("retry settles");
        assert!(refs.cogs_sequence.is_some());
        assert_settled_once(&mint, &event);

        // Idempotency of the *money*: net balances show a single settlement even
        // though attempt 1's hold+capture were created and then reversed. Attempt
        // 1's hold was captured, so its ONLY correct compensation is the revenue
        // reversal — the captured hold is terminal on Mint and is never released
        // (releasing it would be a distinct close of a terminal hold => HoldNotFound).
        // Attempt 2 is the only one whose COGS transfer ever committed.
        assert_eq!(
            mint.effective_calls(Op::ReleaseHold),
            0,
            "a captured hold is terminal and must not be released"
        );
        assert_eq!(mint.effective_calls(Op::ReverseRevenue), 1);
        assert_eq!(mint.effective_calls(Op::TransferCogs), 1);

        let saga = outbox
            .load(&event.request_id)
            .await
            .expect("load")
            .expect("saga");
        assert_eq!(saga.state, SagaState::Settled);
        assert_eq!(saga.attempt, 2);
    }

    #[tokio::test]
    async fn settled_saga_replays_without_new_mint_calls() {
        let mint = Arc::new(FakeMint::default());
        let outbox = Arc::new(MemoryOutbox::new());
        let coordinator = coordinator(mint.clone(), outbox.clone());
        let event = event();

        let first = coordinator.settle(&event).await.expect("first settle");
        let replay = coordinator.settle(&event).await.expect("replay settle");

        assert_eq!(first, replay, "replay returns identical refs");
        assert_settled_once(&mint, &event);
        // No double charge: each forward op ran exactly once, not on the replay.
        assert_eq!(mint.effective_calls(Op::CreateHold), 1);
        assert_eq!(mint.effective_calls(Op::CaptureHold), 1);
        assert_eq!(mint.effective_calls(Op::TransferCogs), 1);
    }

    #[tokio::test]
    async fn resume_from_pending_does_not_redo_committed_steps() {
        // Simulate a process that committed hold+capture, persisted them durably,
        // then crashed before the COGS transfer.
        let mint = Arc::new(FakeMint::default());
        let outbox = Arc::new(MemoryOutbox::new());
        let event = event();

        let hold = mint
            .create_hold(&event, &step_key(&event.request_id, 1, StepKey::Hold))
            .await
            .expect("seed hold");
        let revenue_seq = mint
            .capture_hold(
                &hold.hold_id,
                &event,
                &step_key(&event.request_id, 1, StepKey::Capture),
            )
            .await
            .expect("seed capture");

        let mut saga = SettlementSaga::new(&event.request_id, fixed_clock().now());
        saga.hold_state = StepState::Committed;
        saga.hold_id = Some(hold.hold_id);
        saga.hold_seq = Some(hold.first_seq);
        saga.capture_state = StepState::Committed;
        saga.revenue_seq = Some(revenue_seq);
        outbox.upsert(&saga).await.expect("seed saga");

        // A fresh coordinator (as if after restart) resumes the same attempt.
        let coordinator = coordinator(mint.clone(), outbox.clone());
        let refs = coordinator.settle(&event).await.expect("resume settles");

        assert!(refs.cogs_sequence.is_some());
        assert_settled_once(&mint, &event);
        // Crash-resume must not re-run committed steps.
        assert_eq!(mint.effective_calls(Op::CreateHold), 1);
        assert_eq!(mint.effective_calls(Op::CaptureHold), 1);
        assert_eq!(mint.effective_calls(Op::TransferCogs), 1);
        let saga = outbox
            .load(&event.request_id)
            .await
            .expect("load")
            .expect("saga");
        assert_eq!(saga.state, SagaState::Settled);
        assert_eq!(saga.attempt, 1);
    }

    #[tokio::test]
    async fn incomplete_compensation_is_resumable_to_fully_compensated() {
        // Forward fails at COGS, and the first reversal of the revenue capture
        // also fails — leaving the saga durably mid-compensation.
        let mint = FakeMint::fail(&[Op::TransferCogs, Op::ReverseRevenue]);
        let outbox = Arc::new(MemoryOutbox::new());
        let coordinator = coordinator(mint.clone(), outbox.clone());
        let event = event();

        assert!(coordinator.settle(&event).await.is_err());
        let saga = outbox
            .load(&event.request_id)
            .await
            .expect("load")
            .expect("saga");
        assert_eq!(saga.state, SagaState::Compensating);
        assert_eq!(
            saga.capture_state,
            StepState::Committed,
            "reversal not yet done"
        );

        // The reversal fault clears; the next call must finish compensation.
        mint.stop_failing(Op::ReverseRevenue);
        let result = coordinator.settle(&event).await;
        assert!(result.is_err(), "the attempt still failed to settle");
        assert_fully_compensated(&mint, &event);
        let saga = outbox
            .load(&event.request_id)
            .await
            .expect("load")
            .expect("saga");
        assert_eq!(saga.state, SagaState::Compensated);
    }

    #[tokio::test]
    async fn failure_at_capture_releases_the_hold() {
        // Fault at capture — only the hold has committed; releasing it nets zero.
        let mint = FakeMint::fail(&[Op::CaptureHold]);
        let outbox = Arc::new(MemoryOutbox::new());
        let coordinator = coordinator(mint.clone(), outbox.clone());
        let event = event();

        assert!(coordinator.settle(&event).await.is_err());
        assert_fully_compensated(&mint, &event);
        assert_eq!(mint.effective_calls(Op::ReleaseHold), 1);
        assert_eq!(mint.effective_calls(Op::ReverseRevenue), 0);
        let saga = outbox
            .load(&event.request_id)
            .await
            .expect("load")
            .expect("saga");
        assert_eq!(saga.state, SagaState::Compensated);
    }

    #[tokio::test]
    async fn failed_event_makes_no_movements_and_no_saga() {
        let mint = Arc::new(FakeMint::default());
        let outbox = Arc::new(MemoryOutbox::new());
        let coordinator = coordinator(mint.clone(), outbox.clone());
        let mut failed = event();
        failed.outcome = EventOutcome::Failed;
        failed.revenue_micros = 0;
        failed.cogs_micros = 0;

        let refs = coordinator.settle(&failed).await.expect("failed settle");
        assert_eq!(refs, SettlementRefs::failed_request());
        assert_eq!(mint.effective_calls(Op::CreateHold), 0);
        assert!(outbox
            .load(&failed.request_id)
            .await
            .expect("load")
            .is_none());
    }
}
