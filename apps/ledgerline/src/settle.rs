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
use crate::outbox::{Claim, OutboxStore, SagaState, SettlementSaga, StepState};

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

/// How long a driver's lease on a saga lasts without renewal. Every Mint step
/// renews it first, so only a driver stalled longer than this loses the saga.
pub const DEFAULT_LEASE: chrono::Duration = chrono::Duration::seconds(120);

/// Deterministic settlement coordinator over injectable Mint ops, a durable
/// outbox, and a clock.
///
/// One saga has one driver at a time: `settle` claims the saga's lease in the
/// outbox (another instance or the recovery sweep gets
/// [`LedgerlineError::InProgress`] and moves no money), renews it before every
/// Mint call, and every write is fenced on it, so a driver that lost the lease
/// stops before its next Mint call and can never overwrite newer progress.
#[derive(Clone)]
pub struct SettlementCoordinator {
    ops: Arc<dyn MintOps>,
    outbox: Arc<dyn OutboxStore>,
    clock: Arc<dyn Clock>,
    lease: chrono::Duration,
}

/// A unique lease owner for one `settle` call.
fn owner_token() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("{}-{nanos:x}-{n}", std::process::id())
}

impl SettlementCoordinator {
    pub fn new(ops: Arc<dyn MintOps>, outbox: Arc<dyn OutboxStore>, clock: Arc<dyn Clock>) -> Self {
        Self {
            ops,
            outbox,
            clock,
            lease: DEFAULT_LEASE,
        }
    }

    /// A coordinator whose saga leases last `lease` without renewal.
    pub fn with_lease(self, lease: chrono::Duration) -> Self {
        Self { lease, ..self }
    }

    /// Persist progress and renew the lease; fails with `InProgress` when the
    /// lease was taken over.
    async fn persist(&self, saga: &mut SettlementSaga, owner: &str) -> Result<(), LedgerlineError> {
        let now = self.clock.now();
        saga.updated_at = now;
        self.outbox.save(saga, owner, now + self.lease).await
    }

    /// Renew the lease before a Mint call without recording progress
    /// (`updated_at` keeps meaning "last step taken", which the recovery
    /// sweep's staleness window reads).
    async fn renew(&self, saga: &SettlementSaga, owner: &str) -> Result<(), LedgerlineError> {
        self.outbox
            .save(saga, owner, self.clock.now() + self.lease)
            .await
    }

    async fn run_forward(
        &self,
        saga: &mut SettlementSaga,
        event: &MeterEvent,
        owner: &str,
    ) -> Result<SettlementRefs, LedgerlineError> {
        if saga.hold_state != StepState::Committed {
            self.renew(saga, owner).await?;
            let key = step_key(&saga.request_id, saga.attempt, StepKey::Hold);
            match self.ops.create_hold(event, &key).await {
                Ok(hold) => {
                    saga.hold_id = Some(hold.hold_id);
                    saga.hold_seq = Some(hold.first_seq);
                    saga.hold_state = StepState::Committed;
                    self.persist(saga, owner).await?;
                }
                Err(cause) => return self.begin_compensation(saga, event, cause, owner).await,
            }
        }

        if saga.capture_state != StepState::Committed {
            self.renew(saga, owner).await?;
            let hold_id = saga.hold_id.clone().ok_or_else(|| {
                LedgerlineError::Storage("capture step reached without a hold id".into())
            })?;
            let key = step_key(&saga.request_id, saga.attempt, StepKey::Capture);
            match self.ops.capture_hold(&hold_id, event, &key).await {
                Ok(seq) => {
                    saga.revenue_seq = Some(seq);
                    saga.capture_state = StepState::Committed;
                    self.persist(saga, owner).await?;
                }
                Err(cause) => return self.begin_compensation(saga, event, cause, owner).await,
            }
        }

        if saga.cogs_state != StepState::Committed {
            self.renew(saga, owner).await?;
            let key = step_key(&saga.request_id, saga.attempt, StepKey::Cogs);
            match self.ops.transfer_cogs(event, &key).await {
                Ok(seq) => {
                    saga.cogs_seq = Some(seq);
                    saga.cogs_state = StepState::Committed;
                    self.persist(saga, owner).await?;
                }
                Err(cause) => return self.begin_compensation(saga, event, cause, owner).await,
            }
        }

        saga.state = SagaState::Settled;
        self.persist(saga, owner).await?;
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
        owner: &str,
    ) -> Result<SettlementRefs, LedgerlineError> {
        saga.state = SagaState::Compensating;
        self.persist(saga, owner).await?;
        self.run_compensation(saga, event, owner).await?;
        Err(cause)
    }

    /// Reverse committed steps in strict reverse order. Each reversal is keyed and
    /// idempotent, and progress is persisted after each success, so this is safe
    /// to call repeatedly until the saga reaches `Compensated`.
    async fn run_compensation(
        &self,
        saga: &mut SettlementSaga,
        event: &MeterEvent,
        owner: &str,
    ) -> Result<(), LedgerlineError> {
        if saga.cogs_state == StepState::Committed {
            self.renew(saga, owner).await?;
            let key = step_key(&saga.request_id, saga.attempt, StepKey::CogsReverse);
            self.ops.reverse_cogs(event, &key).await?;
            saga.cogs_state = StepState::Compensated;
            self.persist(saga, owner).await?;
        }

        if saga.capture_state == StepState::Committed {
            self.renew(saga, owner).await?;
            let key = step_key(&saga.request_id, saga.attempt, StepKey::RevenueReverse);
            self.ops.reverse_revenue(event, &key).await?;
            saga.capture_state = StepState::Compensated;
            self.persist(saga, owner).await?;
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
                self.renew(saga, owner).await?;
                let hold_id = saga.hold_id.clone().ok_or_else(|| {
                    LedgerlineError::Storage("hold release reached without a hold id".into())
                })?;
                let key = step_key(&saga.request_id, saga.attempt, StepKey::HoldRelease);
                self.ops.release_hold(&hold_id, event, &key).await?;
            }
            saga.hold_state = StepState::Compensated;
            self.persist(saga, owner).await?;
        }

        saga.state = SagaState::Compensated;
        self.persist(saga, owner).await?;
        Ok(())
    }

    /// Take the saga's lease (creating the saga when it is new). `None` when
    /// another driver holds it.
    async fn claim(
        &self,
        event: &MeterEvent,
        event_json: &str,
        owner: &str,
    ) -> Result<Option<SettlementSaga>, LedgerlineError> {
        // Two rounds: a lost create race is followed by a claim of the winner's row.
        for _ in 0..2 {
            let now = self.clock.now();
            match self
                .outbox
                .claim(&event.request_id, owner, now, now + self.lease)
                .await?
            {
                Claim::Claimed(saga) => return Ok(Some(saga)),
                Claim::Busy => return Ok(None),
                Claim::Missing => {
                    let saga = SettlementSaga::new(&event.request_id, event_json, now);
                    if self
                        .outbox
                        .insert_claimed(&saga, owner, now + self.lease)
                        .await?
                    {
                        return Ok(Some(saga));
                    }
                }
            }
        }
        Ok(None)
    }

    /// The body of `settle` while holding the lease.
    async fn drive(
        &self,
        existing: SettlementSaga,
        event: &MeterEvent,
        event_json: &str,
        owner: &str,
    ) -> Result<SettlementRefs, LedgerlineError> {
        if existing.event_json != event_json {
            return Err(LedgerlineError::IdempotencyConflict);
        }
        let mut saga = match existing.state {
            // Already fully settled: idempotent replay, no new Mint calls.
            SagaState::Settled => return Ok(existing.refs()),
            // A prior attempt failed mid-reversal: finish it, then report.
            SagaState::Compensating => {
                let mut resumed = existing;
                self.run_compensation(&mut resumed, event, owner).await?;
                return Err(LedgerlineError::Mint(
                    "settlement was compensated after a prior failure".into(),
                ));
            }
            // Prior attempt cleanly rolled back: retry on a fresh attempt. Only
            // the lease holder gets here, so no older attempt is still running.
            SagaState::Compensated => existing.next_attempt(self.clock.now()),
            // Prior attempt crashed mid-flight (or is brand new): resume it.
            SagaState::Pending => existing,
        };
        saga.state = SagaState::Pending;
        self.persist(&mut saga, owner).await?;
        self.run_forward(&mut saga, event, owner).await
    }
}

#[async_trait]
impl MintSettlement for SettlementCoordinator {
    async fn settle(&self, event: &MeterEvent) -> Result<SettlementRefs, LedgerlineError> {
        // Failed requests never move money; there is nothing to settle.
        if event.outcome == EventOutcome::Failed {
            return Ok(SettlementRefs::failed_request());
        }

        let event_json = serde_json::to_string(event).map_err(|error| {
            LedgerlineError::Storage(format!("serializing event binding: {error}"))
        })?;
        let owner = owner_token();
        let Some(existing) = self.claim(event, &event_json, &owner).await? else {
            return Err(LedgerlineError::InProgress);
        };
        let result = self.drive(existing, event, &event_json, &owner).await;
        if let Err(error) = self.outbox.release(&event.request_id, &owner).await {
            // The lease simply expires; nothing else depends on the release.
            tracing::warn!(request_id = %event.request_id, %error, "could not release settlement lease");
        }
        result
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
    use super::*;
    use crate::outbox::MemoryOutbox;
    use crate::test_support::{
        assert_fully_compensated, assert_settled_once, coordinator, event, fixed_clock, FakeMint,
        Op,
    };

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
    async fn restart_rejects_changed_financial_event_for_same_request() {
        let mint = Arc::new(FakeMint::default());
        let outbox = Arc::new(MemoryOutbox::new());
        let coordinator = coordinator(mint.clone(), outbox);
        let event = event();
        coordinator.settle(&event).await.expect("first settle");

        let mut changed = event.clone();
        changed.revenue_micros += 1;
        assert_eq!(
            coordinator.settle(&changed).await,
            Err(LedgerlineError::IdempotencyConflict)
        );
        assert_settled_once(&mint, &event);
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

        let mut saga = SettlementSaga::new(
            &event.request_id,
            serde_json::to_string(&event).unwrap(),
            fixed_clock().now(),
        );
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
