//! Autonomous settlement recovery worker.
//!
//! The [`SettlementCoordinator`](crate::settle::SettlementCoordinator) already
//! recovers a stuck saga when a *new inbound request* for the same `request_id`
//! calls `settle` again — it resumes a `Pending` forward or finishes a
//! `Compensating` reversal. But absent that inbound retry, a saga left durably
//! incomplete by a crash (process died between a Mint call and the local commit)
//! would sit forever: money reserved-but-not-captured, or a half-run reversal.
//!
//! This worker closes that gap. On a fixed interval it asks the outbox for
//! incomplete sagas untouched for longer than a staleness window (so it never
//! races a live request), deserializes each saga's authorized financial event,
//! and drives it back through the coordinator's idempotent `settle` path:
//!
//! - a `Pending` saga completes its forward trio (idempotent per-attempt keys
//!   make already-committed steps no-ops); and
//! - a `Compensating` saga finishes its reversal to `Compensated`.
//!
//! Mint remains the money authority and every step is idempotency-keyed, so a
//! sweep that overlaps a just-resumed request cannot double-charge or
//! double-reverse.

use std::sync::Arc;
use std::time::Duration;

use tokio::task::JoinHandle;

use crate::domain::{MarginRecord, MeterEvent};
use crate::error::LedgerlineError;
use crate::mint::MintSettlement;
use crate::outbox::OutboxStore;
use crate::settle::Clock;
use crate::store::EventStore;

/// Recovery worker cadence and safety window.
#[derive(Clone, Copy, Debug)]
pub struct RecoveryConfig {
    /// How often a sweep runs.
    pub interval: Duration,
    /// Only sagas untouched for at least this long are considered stuck. Keeps
    /// the worker from racing a live, in-flight settlement.
    pub stale_after: chrono::Duration,
    /// Maximum sagas advanced per sweep.
    pub batch: i64,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(30),
            stale_after: chrono::Duration::seconds(60),
            batch: 100,
        }
    }
}

/// Spawn the background recovery loop. The returned handle keeps the task alive;
/// dropping it aborts the worker.
///
/// Each tick runs two idempotent sweeps: [`recover_once`] drives stuck forward or
/// mid-compensation sagas to a terminal state, then [`reconcile_projections`]
/// backfills the margin projection for any settled saga a crash left unprojected.
pub fn spawn_recovery_worker(
    settler: Arc<dyn MintSettlement>,
    store: Arc<dyn EventStore>,
    outbox: Arc<dyn OutboxStore>,
    clock: Arc<dyn Clock>,
    config: RecoveryConfig,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(config.interval);
        // Skip missed ticks rather than bursting after a slow sweep.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            match recover_once(settler.as_ref(), outbox.as_ref(), clock.as_ref(), &config).await {
                Ok(0) => {}
                Ok(driven) => tracing::info!(driven, "settlement recovery advanced stuck sagas"),
                Err(error) => tracing::warn!(%error, "settlement recovery sweep failed"),
            }
            match reconcile_projections(store.as_ref(), outbox.as_ref(), clock.as_ref(), &config)
                .await
            {
                Ok(0) => {}
                Ok(reconciled) => {
                    tracing::info!(
                        reconciled,
                        "settlement recovery backfilled margin projections"
                    )
                }
                Err(error) => tracing::warn!(%error, "projection reconciliation sweep failed"),
            }
        }
    })
}

/// Run a single recovery sweep. Returns how many stuck sagas were driven.
///
/// Exposed (not just the spawn loop) so it can be invoked deterministically.
pub async fn recover_once(
    settler: &dyn MintSettlement,
    outbox: &dyn OutboxStore,
    clock: &dyn Clock,
    config: &RecoveryConfig,
) -> Result<usize, LedgerlineError> {
    let cutoff = clock.now() - config.stale_after;
    let stuck = outbox.list_incomplete(cutoff, config.batch).await?;

    let mut driven = 0usize;
    for saga in stuck {
        let event: MeterEvent = match serde_json::from_str(&saga.event_json) {
            Ok(event) => event,
            Err(error) => {
                // A saga we cannot decode is not recoverable by replay; surface it
                // loudly and skip rather than abort the whole sweep.
                tracing::error!(
                    request_id = %saga.request_id,
                    %error,
                    "skipping saga with undecodable event binding"
                );
                continue;
            }
        };

        match settler.settle(&event).await {
            // Forward path completed (Pending -> Settled) or the saga was already
            // terminal by the time we re-drove it.
            Ok(_) => {
                driven += 1;
                tracing::info!(request_id = %saga.request_id, "recovered incomplete settlement");
            }
            // A resumed Compensating saga reports its terminal compensation as a
            // Mint error by contract — that IS a successful recovery (the money
            // netted to zero), so count it and move on.
            Err(LedgerlineError::Mint(_)) => {
                driven += 1;
                tracing::info!(
                    request_id = %saga.request_id,
                    "finished compensation for a failed settlement"
                );
            }
            // Another driver (a live request or another instance's sweep)
            // holds the saga's lease: it is not stuck, leave it alone.
            Err(LedgerlineError::InProgress) => {
                tracing::debug!(request_id = %saga.request_id, "saga is being driven elsewhere");
            }
            // A transient failure (Mint/store still unhealthy): leave the saga
            // durable and retry it on the next sweep.
            Err(error) => {
                tracing::warn!(
                    request_id = %saga.request_id,
                    %error,
                    "recovery could not yet advance saga; will retry"
                );
            }
        }
    }
    Ok(driven)
}

/// Run a single projection reconciliation sweep. Returns how many missing margin
/// projections were backfilled.
///
/// The settlement coordinator can leave a saga durably `Settled` — money fully
/// moved on Mint — while the request's `margin_events` projection is absent,
/// either because the ingest process crashed between `settle` and `store.save`,
/// or because [`recover_once`] drove a crashed forward saga to `Settled` (the
/// coordinator moves money but never writes the projection). Left alone, that
/// request's gross margin is invisible forever, so this sweep rebuilds it from
/// the saga's authorized event binding and Mint references.
///
/// It is money-safe: it never touches Mint, only writes the read-model
/// projection, and `EventStore::save` is idempotent by `idempotency_key`, so a
/// racing client retry can never produce a double projection.
pub async fn reconcile_projections(
    store: &dyn EventStore,
    outbox: &dyn OutboxStore,
    clock: &dyn Clock,
    config: &RecoveryConfig,
) -> Result<usize, LedgerlineError> {
    let cutoff = clock.now() - config.stale_after;
    let unprojected = outbox
        .list_unprojected_settled(cutoff, config.batch)
        .await?;

    let mut reconciled = 0usize;
    for saga in unprojected {
        // A concurrent client retry (or a prior sweep) may already have written
        // the projection; the Postgres anti-join excludes those, but the memory
        // outbox cannot, and either backend can race a live retry. Skip if the
        // projection is already present so the sweep does no redundant work and
        // its count reflects only genuine backfills.
        if store.by_request_id(&saga.request_id).await?.is_some() {
            continue;
        }

        let event: MeterEvent = match serde_json::from_str(&saga.event_json) {
            Ok(event) => event,
            Err(error) => {
                // Undecodable binding: not reconstructable by replay. Surface it
                // loudly and skip rather than abort the whole sweep.
                tracing::error!(
                    request_id = %saga.request_id,
                    %error,
                    "skipping settled saga with undecodable event binding"
                );
                continue;
            }
        };

        let record = match MarginRecord::new(event, saga.refs(), clock.now()) {
            Ok(record) => record,
            Err(error) => {
                tracing::error!(
                    request_id = %saga.request_id,
                    %error,
                    "skipping settled saga whose margin could not be reconstructed"
                );
                continue;
            }
        };

        // Idempotent by `idempotency_key`: a racing retry that inserted first
        // makes this a no-op read, never a second projection or money movement.
        store.save(&record).await?;
        reconciled += 1;
        tracing::info!(
            request_id = %saga.request_id,
            "backfilled missing margin projection for a settled request"
        );
    }
    Ok(reconciled)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};

    use crate::mint::MintOps;
    use crate::outbox::{MemoryOutbox, SagaState, SettlementSaga, StepState};
    use crate::server::{ingest_event, AppState};
    use crate::settle::{step_key, StepKey};
    use crate::store::MemoryStore;
    use crate::test_support::{
        assert_fully_compensated, assert_settled_once, coordinator, event, fixed_clock, FakeMint,
        Op,
    };

    /// An instant safely older than the default staleness window (60s before the
    /// fixed clock's "now"), so a seeded saga is treated as genuinely stuck.
    fn stale() -> DateTime<Utc> {
        fixed_clock().now() - chrono::Duration::hours(1)
    }

    /// Seed a fresh, all-steps-`Pending` saga (a crash right after intent was
    /// persisted, before any Mint call) for `event`, timestamped `when`.
    async fn seed_pending(outbox: &MemoryOutbox, event: &MeterEvent, when: DateTime<Utc>) {
        let saga = SettlementSaga::new(
            &event.request_id,
            serde_json::to_string(event).expect("serialize event"),
            when,
        );
        outbox.upsert(&saga).await.expect("seed saga");
    }

    /// Rewrite a saga's `updated_at` (immutably) so the staleness window sees it
    /// as stuck, without disturbing any step state.
    async fn backdate(outbox: &MemoryOutbox, request_id: &str, when: DateTime<Utc>) {
        let saga = outbox
            .load(request_id)
            .await
            .expect("load")
            .expect("saga present");
        let saga = SettlementSaga {
            updated_at: when,
            ..saga
        };
        outbox.upsert(&saga).await.expect("backdate saga");
    }

    fn config() -> RecoveryConfig {
        RecoveryConfig::default()
    }

    /// A `Pending` saga past its staleness window is driven forward to a terminal
    /// `Settled` state, and the money moved exactly once.
    #[tokio::test]
    async fn pending_saga_past_staleness_is_recovered_to_settled() {
        let mint = Arc::new(FakeMint::default());
        let outbox = Arc::new(MemoryOutbox::new());
        let coordinator = coordinator(mint.clone(), outbox.clone());
        let clock = fixed_clock();
        let event = event();

        seed_pending(&outbox, &event, stale()).await;

        let driven = recover_once(&coordinator, outbox.as_ref(), clock.as_ref(), &config())
            .await
            .expect("sweep");

        assert_eq!(driven, 1, "the one stuck Pending saga was driven");
        let saga = outbox
            .load(&event.request_id)
            .await
            .expect("load")
            .expect("saga");
        assert_eq!(saga.state, SagaState::Settled, "driven to terminal success");
        assert_settled_once(&mint, &event);
    }

    /// A `Pending` saga that crashed mid-forward (hold + capture already committed
    /// durably) is resumed by the sweep without re-running the committed steps —
    /// the per-step idempotency the recovery path relies on.
    #[tokio::test]
    async fn pending_saga_resumes_without_redoing_committed_steps() {
        let mint = Arc::new(FakeMint::default());
        let outbox = Arc::new(MemoryOutbox::new());
        let event = event();

        // Simulate the durable state left by a crash after hold + capture.
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
        let base = SettlementSaga::new(
            &event.request_id,
            serde_json::to_string(&event).expect("serialize"),
            stale(),
        );
        let saga = SettlementSaga {
            hold_state: StepState::Committed,
            hold_id: Some(hold.hold_id),
            hold_seq: Some(hold.first_seq),
            capture_state: StepState::Committed,
            revenue_seq: Some(revenue_seq),
            ..base
        };
        outbox.upsert(&saga).await.expect("seed saga");

        let coordinator = coordinator(mint.clone(), outbox.clone());
        let clock = fixed_clock();
        let driven = recover_once(&coordinator, outbox.as_ref(), clock.as_ref(), &config())
            .await
            .expect("sweep");

        assert_eq!(driven, 1);
        assert_settled_once(&mint, &event);
        // Committed steps were not re-run; only the missing COGS step executed.
        assert_eq!(mint.effective_calls(Op::CreateHold), 1);
        assert_eq!(mint.effective_calls(Op::CaptureHold), 1);
        assert_eq!(mint.effective_calls(Op::TransferCogs), 1);
        let saga = outbox
            .load(&event.request_id)
            .await
            .expect("load")
            .expect("saga");
        assert_eq!(saga.state, SagaState::Settled);
        assert_eq!(saga.attempt, 1, "resumed the same attempt, not a fresh one");
    }

    /// A `Compensating` saga (crashed mid-reversal) is driven to terminal
    /// `Compensated`, and every account nets back to zero — the money invariant.
    #[tokio::test]
    async fn compensating_saga_is_driven_to_compensated_and_money_neutral() {
        // Force the saga durably into `Compensating`: the forward path fails at
        // COGS, and the first reversal (revenue) also fails, so compensation is
        // left half-done with money NOT yet neutral.
        let mint = FakeMint::fail(&[Op::TransferCogs, Op::ReverseRevenue]);
        let outbox = Arc::new(MemoryOutbox::new());
        let coordinator = coordinator(mint.clone(), outbox.clone());
        let clock = fixed_clock();
        let event = event();

        assert!(coordinator.settle(&event).await.is_err());
        let saga = outbox
            .load(&event.request_id)
            .await
            .expect("load")
            .expect("saga");
        assert_eq!(
            saga.state,
            SagaState::Compensating,
            "precondition: stuck mid-reversal"
        );
        assert_ne!(
            mint.balance(&event.customer_account),
            0,
            "precondition: money is NOT yet neutral"
        );

        // The window now sees it as stuck, and Mint has recovered for the reversal.
        backdate(&outbox, &event.request_id, stale()).await;
        mint.stop_failing(Op::ReverseRevenue);

        let driven = recover_once(&coordinator, outbox.as_ref(), clock.as_ref(), &config())
            .await
            .expect("sweep");

        assert_eq!(driven, 1, "the Compensating saga was driven to completion");
        let saga = outbox
            .load(&event.request_id)
            .await
            .expect("load")
            .expect("saga");
        assert_eq!(saga.state, SagaState::Compensated);
        assert_fully_compensated(&mint, &event);
    }

    /// A corrupt / undecodable saga row does not crash the worker: it is skipped
    /// and the sweep continues to drive the healthy sagas behind it.
    #[tokio::test]
    async fn undecodable_saga_row_is_skipped_and_recovery_continues() {
        let mint = Arc::new(FakeMint::default());
        let outbox = Arc::new(MemoryOutbox::new());
        let coordinator = coordinator(mint.clone(), outbox.clone());
        let clock = fixed_clock();

        // A corrupt row, timestamped older so it is processed first in the sweep.
        let corrupt = SettlementSaga::new(
            "corrupt-req",
            "{ this is not a MeterEvent",
            stale() - chrono::Duration::minutes(5),
        );
        outbox.upsert(&corrupt).await.expect("seed corrupt");

        // A healthy stuck Pending saga behind it.
        let good = event();
        seed_pending(&outbox, &good, stale()).await;

        let driven = recover_once(&coordinator, outbox.as_ref(), clock.as_ref(), &config())
            .await
            .expect("sweep must not error on a bad row");

        assert_eq!(
            driven, 1,
            "only the decodable saga was driven; the corrupt one was skipped"
        );
        let good_saga = outbox
            .load(&good.request_id)
            .await
            .expect("load")
            .expect("saga");
        assert_eq!(
            good_saga.state,
            SagaState::Settled,
            "one bad row did not wedge the sweep"
        );
        assert_settled_once(&mint, &good);
        // The corrupt row is left untouched (quarantined), never driven or crashed.
        let corrupt_saga = outbox
            .load("corrupt-req")
            .await
            .expect("load")
            .expect("saga");
        assert_eq!(corrupt_saga.state, SagaState::Pending);
    }

    /// Running the sweep twice over the same seeded state does not double-drive:
    /// the money moves exactly once and terminal sagas are not re-picked.
    #[tokio::test]
    async fn recover_once_is_idempotent_across_repeated_sweeps() {
        let mint = Arc::new(FakeMint::default());
        let outbox = Arc::new(MemoryOutbox::new());
        let coordinator = coordinator(mint.clone(), outbox.clone());
        let clock = fixed_clock();
        let event = event();
        let config = config();

        seed_pending(&outbox, &event, stale()).await;

        let first = recover_once(&coordinator, outbox.as_ref(), clock.as_ref(), &config)
            .await
            .expect("first sweep");
        let second = recover_once(&coordinator, outbox.as_ref(), clock.as_ref(), &config)
            .await
            .expect("second sweep");

        assert_eq!(first, 1, "first sweep drives the stuck saga");
        assert_eq!(
            second, 0,
            "second sweep re-drives nothing: the saga is now terminal"
        );
        // The money moved exactly once despite two sweeps.
        assert_settled_once(&mint, &event);
        assert_eq!(mint.effective_calls(Op::CreateHold), 1);
        assert_eq!(mint.effective_calls(Op::CaptureHold), 1);
        assert_eq!(mint.effective_calls(Op::TransferCogs), 1);
    }

    /// A saga still inside the staleness window is left alone, so the worker never
    /// races a live, in-flight settlement for the same request.
    #[tokio::test]
    async fn fresh_pending_saga_within_stale_window_is_not_recovered() {
        let mint = Arc::new(FakeMint::default());
        let outbox = Arc::new(MemoryOutbox::new());
        let coordinator = coordinator(mint.clone(), outbox.clone());
        let clock = fixed_clock();
        let event = event();

        // updated_at == now: inside the window, possibly a live request in flight.
        seed_pending(&outbox, &event, clock.now()).await;

        let driven = recover_once(&coordinator, outbox.as_ref(), clock.as_ref(), &config())
            .await
            .expect("sweep");

        assert_eq!(driven, 0, "recovery must not touch a fresh saga");
        let saga = outbox
            .load(&event.request_id)
            .await
            .expect("load")
            .expect("saga");
        assert_eq!(
            saga.state,
            SagaState::Pending,
            "left untouched for the live request"
        );
        assert_eq!(mint.effective_calls(Op::CreateHold), 0);
    }

    /// If Mint is still unhealthy during the sweep, a `Compensating` saga is NOT
    /// lost or partially moved: it stays durably `Compensating` for a later retry,
    /// and a subsequent sweep (once Mint recovers) finishes it money-neutral.
    #[tokio::test]
    async fn compensating_saga_still_failing_stays_durable_for_retry() {
        let mint = FakeMint::fail(&[Op::TransferCogs, Op::ReverseRevenue]);
        let outbox = Arc::new(MemoryOutbox::new());
        let coordinator = coordinator(mint.clone(), outbox.clone());
        let clock = fixed_clock();
        let event = event();

        assert!(coordinator.settle(&event).await.is_err());
        backdate(&outbox, &event.request_id, stale()).await;

        let customer_before = mint.balance(&event.customer_account);
        let revenue_before = mint.balance(&event.revenue_account);

        // Reversal is STILL failing during this sweep.
        let _ = recover_once(&coordinator, outbox.as_ref(), clock.as_ref(), &config())
            .await
            .expect("sweep must not error");

        let saga = outbox
            .load(&event.request_id)
            .await
            .expect("load")
            .expect("saga");
        assert_eq!(
            saga.state,
            SagaState::Compensating,
            "still stuck: retained durably for a later sweep, not lost"
        );
        assert_eq!(
            mint.balance(&event.customer_account),
            customer_before,
            "no new money moved while Mint is down"
        );
        assert_eq!(mint.balance(&event.revenue_account), revenue_before);

        // Once Mint recovers, a later sweep finishes compensation, money-neutral.
        mint.stop_failing(Op::ReverseRevenue);
        let driven = recover_once(&coordinator, outbox.as_ref(), clock.as_ref(), &config())
            .await
            .expect("recovery sweep");
        assert_eq!(driven, 1);
        let saga = outbox
            .load(&event.request_id)
            .await
            .expect("load")
            .expect("saga");
        assert_eq!(saga.state, SagaState::Compensated);
        assert_fully_compensated(&mint, &event);
    }

    // ------------------------------------------------------------------------
    // Projection reconciliation: close the money-visibility gap where a request
    // is fully settled on Mint but its margin projection never landed.
    // ------------------------------------------------------------------------

    /// The flagship durability gap: settlement commits (money moved, saga
    /// `Settled`) but the ingest process crashes before writing the margin row.
    /// The reconciliation sweep backfills exactly that projection from the saga's
    /// authorized event binding, moving no additional money.
    #[tokio::test]
    async fn reconcile_backfills_settled_saga_missing_its_projection() {
        let mint = Arc::new(FakeMint::default());
        let outbox = Arc::new(MemoryOutbox::new());
        let store = MemoryStore::default();
        let coordinator = coordinator(mint.clone(), outbox.clone());
        let clock = fixed_clock();
        let event = event();

        // Server settles first, then persists the projection. Simulate a crash in
        // that window: settlement commits but `store.save` never runs.
        coordinator.settle(&event).await.expect("settle");
        assert_settled_once(&mint, &event);
        assert!(
            store
                .by_request_id(&event.request_id)
                .await
                .expect("query")
                .is_none(),
            "precondition: the margin projection is missing"
        );

        // Age the settled saga past the staleness window (a real crash, not a
        // live request mid-flight).
        backdate(&outbox, &event.request_id, stale()).await;

        let reconciled = reconcile_projections(&store, outbox.as_ref(), clock.as_ref(), &config())
            .await
            .expect("reconcile sweep");

        assert_eq!(
            reconciled, 1,
            "the one unprojected settled saga was backfilled"
        );
        let record = store
            .by_request_id(&event.request_id)
            .await
            .expect("query")
            .expect("projection now present");
        assert_eq!(record.margin_micros, 300_000, "revenue 900k - cogs 600k");
        assert!(
            record.settlement.revenue_hold_id.is_some()
                && record.settlement.revenue_sequence.is_some()
                && record.settlement.cogs_sequence.is_some(),
            "reconstructed projection carries the saga's Mint references"
        );
        // Reconciliation writes only the read model; it never touches Mint.
        assert_settled_once(&mint, &event);
    }

    /// Reconciliation is idempotent: a second sweep over the same state backfills
    /// nothing and never writes a duplicate projection.
    #[tokio::test]
    async fn reconcile_is_idempotent_across_sweeps() {
        let mint = Arc::new(FakeMint::default());
        let outbox = Arc::new(MemoryOutbox::new());
        let store = MemoryStore::default();
        let coordinator = coordinator(mint.clone(), outbox.clone());
        let clock = fixed_clock();
        let event = event();

        coordinator.settle(&event).await.expect("settle");
        backdate(&outbox, &event.request_id, stale()).await;

        let first = reconcile_projections(&store, outbox.as_ref(), clock.as_ref(), &config())
            .await
            .expect("first reconcile");
        let second = reconcile_projections(&store, outbox.as_ref(), clock.as_ref(), &config())
            .await
            .expect("second reconcile");

        assert_eq!(first, 1, "first sweep backfills the missing projection");
        assert_eq!(
            second, 0,
            "second sweep finds the projection already present"
        );
        assert_settled_once(&mint, &event);
    }

    /// A settled saga whose projection already exists (the normal, no-crash path)
    /// is never re-projected by the sweep.
    #[tokio::test]
    async fn reconcile_skips_already_projected_settled_saga() {
        let mint = Arc::new(FakeMint::default());
        let outbox = Arc::new(MemoryOutbox::new());
        let store = MemoryStore::default();
        let coordinator = coordinator(mint.clone(), outbox.clone());
        let clock = fixed_clock();
        let event = event();

        // The full happy path: settle AND persist the projection.
        let refs = coordinator.settle(&event).await.expect("settle");
        let record = MarginRecord::new(event.clone(), refs, clock.now()).expect("record");
        let (_, created) = store.save(&record).await.expect("save");
        assert!(created, "projection written on the normal path");
        backdate(&outbox, &event.request_id, stale()).await;

        let reconciled = reconcile_projections(&store, outbox.as_ref(), clock.as_ref(), &config())
            .await
            .expect("reconcile sweep");

        assert_eq!(
            reconciled, 0,
            "nothing to backfill when the projection exists"
        );
    }

    /// A freshly settled saga still inside the staleness window is left alone, so
    /// the sweep never races a live request about to write its own projection.
    #[tokio::test]
    async fn reconcile_respects_the_staleness_window() {
        let mint = Arc::new(FakeMint::default());
        let outbox = Arc::new(MemoryOutbox::new());
        let store = MemoryStore::default();
        let coordinator = coordinator(mint.clone(), outbox.clone());
        let clock = fixed_clock();
        let event = event();

        // settle() stamps the saga at the fixed clock's "now": inside the window.
        coordinator.settle(&event).await.expect("settle");

        let reconciled = reconcile_projections(&store, outbox.as_ref(), clock.as_ref(), &config())
            .await
            .expect("reconcile sweep");

        assert_eq!(reconciled, 0, "a fresh settled saga is not yet reconciled");
        assert!(
            store
                .by_request_id(&event.request_id)
                .await
                .expect("query")
                .is_none(),
            "projection left for the live request to write"
        );
    }

    /// End-to-end money-visibility invariant: a crash between `settle` and
    /// `store.save` is healed by the reconciliation sweep with exactly one margin
    /// row, the money moved exactly once, and a later client retry through the
    /// full ingest path is idempotent — no second settlement, no second row.
    #[tokio::test]
    async fn crash_between_settle_and_save_is_reconciled_and_retry_is_idempotent() {
        let mint = Arc::new(FakeMint::default());
        let outbox = Arc::new(MemoryOutbox::new());
        let store = Arc::new(MemoryStore::default());
        let coordinator = coordinator(mint.clone(), outbox.clone());
        let clock = fixed_clock();
        let event = event();

        // Crash simulation: the coordinator settles (money moves, saga Settled),
        // then the process dies before the ingest handler can persist the row.
        coordinator.settle(&event).await.expect("settle");
        assert_settled_once(&mint, &event);
        assert_eq!(mint.effective_calls(Op::CreateHold), 1);

        // The recovery worker backfills exactly one projection.
        backdate(&outbox, &event.request_id, stale()).await;
        let reconciled =
            reconcile_projections(store.as_ref(), outbox.as_ref(), clock.as_ref(), &config())
                .await
                .expect("reconcile sweep");
        assert_eq!(reconciled, 1);

        // A client that finally retries hits the full ingest path. It must be a
        // pure idempotent replay: no new Mint movement, no duplicate projection.
        let state = AppState {
            mint: Arc::new(coordinator.clone()),
            store: store.clone(),
            api_key: Arc::from("secret"),
            policy: Arc::new(crate::policy::AccountPolicy::unrestricted()),
        };
        let (replayed, created) = ingest_event(&state, event.clone())
            .await
            .expect("retry ingest");
        assert!(!created, "retry does not create a second projection");
        assert!(replayed.idempotent_replay, "retry is an idempotent replay");
        assert_eq!(replayed.margin_micros, 300_000);

        // Money moved exactly once across settle + reconcile + retry.
        assert_settled_once(&mint, &event);
        assert_eq!(mint.effective_calls(Op::CreateHold), 1);
        assert_eq!(mint.effective_calls(Op::CaptureHold), 1);
        assert_eq!(mint.effective_calls(Op::TransferCogs), 1);
    }
}
