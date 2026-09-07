//! Integration tests that drive Ledgerline's REAL Postgres settlement outbox and
//! margin store through the durable [`SettlementCoordinator`] and the projection
//! reconciliation sweep.
//!
//! The crate's unit tests prove the saga against `MemoryOutbox`/`MemoryStore`;
//! these prove the same money invariants — atomic-or-compensated settlement,
//! terminal-hold correctness, resumable compensation, and settled-but-unprojected
//! backfill — hold when the saga state is round-tripped through Postgres.
//!
//! They are gated on `DATABASE_URL`. Without it, each test is a no-op so the
//! default `cargo test` stays green on machines with no database. To exercise
//! them:
//!
//! ```bash
//! cd apps/ledgerline
//! DATABASE_URL=postgres://user@localhost:5432/ledgerline cargo test --test pg_settlement
//! ```

mod common;

use std::sync::Arc;

use chrono::{Duration, Utc};
use common::{
    assert_net_zero, assert_settled_once, event_with_request_id, unique_tag, FakeMint, Op,
};
use ledgerline::domain::MarginRecord;
use ledgerline::mint::MintSettlement;
use ledgerline::outbox::{OutboxStore, PostgresOutbox, SagaState, SettlementSaga, StepState};
use ledgerline::recovery::{reconcile_projections, RecoveryConfig};
use ledgerline::settle::{SettlementCoordinator, SystemClock};
use ledgerline::store::{EventStore, PostgresStore};

/// Connect and migrate the real database, or return `None` when `DATABASE_URL`
/// is unset so the suite is a no-op off a database.
async fn store_or_skip() -> Option<PostgresStore> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let store = PostgresStore::connect(&url)
        .await
        .expect("connect to DATABASE_URL");
    store.migrate().await.expect("run migrations");
    Some(store)
}

fn coordinator(mint: Arc<FakeMint>, outbox: Arc<dyn OutboxStore>) -> SettlementCoordinator {
    SettlementCoordinator::new(mint, outbox, Arc::new(SystemClock))
}

/// Rewrite a saga's `updated_at` so the staleness window treats it as stuck,
/// without disturbing any step state.
async fn backdate(outbox: &dyn OutboxStore, request_id: &str) {
    let saga = outbox
        .load(request_id)
        .await
        .expect("load")
        .expect("saga present");
    let stale = SettlementSaga {
        updated_at: Utc::now() - Duration::hours(1),
        ..saga
    };
    outbox.upsert(&stale).await.expect("backdate");
}

/// Scenario A: COGS transfer fails after hold+capture commit. The captured hold
/// is terminal on Mint, so compensation reverses the revenue and never releases
/// the hold; the saga reaches `Compensated` in Postgres. A retry after the fault
/// clears settles exactly once on a fresh attempt.
#[tokio::test]
async fn pg_cogs_failure_compensates_via_reverse_revenue_then_retry_settles_once() {
    let Some(store) = store_or_skip().await else {
        return;
    };
    let outbox: Arc<dyn OutboxStore> = Arc::new(PostgresOutbox::new(store.pool().clone()));
    let event = event_with_request_id(&unique_tag("req-A"));

    let mint = FakeMint::fail(&[Op::TransferCogs]);
    let coord = coordinator(mint.clone(), outbox.clone());

    assert!(
        coord.settle(&event).await.is_err(),
        "settlement must surface the injected COGS failure"
    );
    assert_net_zero(&mint, &event);
    assert_eq!(
        mint.effective_calls(Op::ReverseRevenue),
        1,
        "reverse_revenue is the compensation for a captured hold"
    );
    assert_eq!(
        mint.effective_calls(Op::ReleaseHold),
        0,
        "a captured hold is terminal and must never be released"
    );

    let saga = outbox.load(&event.request_id).await.unwrap().unwrap();
    assert_eq!(saga.state, SagaState::Compensated, "did not wedge in PG");
    assert_eq!(saga.hold_state, StepState::Compensated);
    assert_eq!(saga.capture_state, StepState::Compensated);
    assert_eq!(saga.cogs_state, StepState::Pending);

    // Fault clears: the retry settles cleanly, exactly once, on attempt 2.
    mint.clear_failures();
    let refs = coord.settle(&event).await.expect("retry settles");
    assert!(refs.cogs_sequence.is_some());
    assert_settled_once(&mint, &event);
    let saga = outbox.load(&event.request_id).await.unwrap().unwrap();
    assert_eq!(saga.state, SagaState::Settled);
    assert_eq!(saga.attempt, 2);
    assert_eq!(
        mint.effective_calls(Op::TransferCogs),
        1,
        "COGS committed exactly once across both attempts"
    );
}

/// Scenario B: capture fails, so only the uncaptured hold is committed; releasing
/// it nets every account to zero and the PG saga reaches `Compensated`.
#[tokio::test]
async fn pg_capture_failure_releases_uncaptured_hold() {
    let Some(store) = store_or_skip().await else {
        return;
    };
    let outbox: Arc<dyn OutboxStore> = Arc::new(PostgresOutbox::new(store.pool().clone()));
    let event = event_with_request_id(&unique_tag("req-B"));

    let mint = FakeMint::fail(&[Op::CaptureHold]);
    let coord = coordinator(mint.clone(), outbox.clone());

    assert!(coord.settle(&event).await.is_err());
    assert_net_zero(&mint, &event);
    assert_eq!(mint.effective_calls(Op::ReleaseHold), 1);
    assert_eq!(mint.effective_calls(Op::ReverseRevenue), 0);
    let saga = outbox.load(&event.request_id).await.unwrap().unwrap();
    assert_eq!(saga.state, SagaState::Compensated);
    assert_eq!(saga.hold_state, StepState::Compensated);
}

/// Scenario C: the forward path fails at COGS AND the first reversal also fails,
/// so the saga wedges durably `Compensating` in Postgres and is surfaced by
/// `list_incomplete`. Once the reversal fault clears, a resume finishes it to
/// `Compensated`, money-neutral.
#[tokio::test]
async fn pg_wedged_compensation_persists_and_resumes_to_compensated() {
    let Some(store) = store_or_skip().await else {
        return;
    };
    let outbox: Arc<dyn OutboxStore> = Arc::new(PostgresOutbox::new(store.pool().clone()));
    let event = event_with_request_id(&unique_tag("req-C"));

    let mint = FakeMint::fail(&[Op::TransferCogs, Op::ReverseRevenue]);
    let coord = coordinator(mint.clone(), outbox.clone());

    assert!(coord.settle(&event).await.is_err());
    let saga = outbox.load(&event.request_id).await.unwrap().unwrap();
    assert_eq!(saga.state, SagaState::Compensating, "durably mid-reversal");
    assert_eq!(saga.capture_state, StepState::Committed);

    let stuck = outbox
        .list_incomplete(Utc::now() + Duration::seconds(60), 100)
        .await
        .unwrap();
    assert!(
        stuck.iter().any(|s| s.request_id == event.request_id),
        "list_incomplete surfaces the Compensating saga from PG"
    );

    mint.stop_failing(Op::ReverseRevenue);
    assert!(
        coord.settle(&event).await.is_err(),
        "the attempt still failed to settle"
    );
    assert_net_zero(&mint, &event);
    let saga = outbox.load(&event.request_id).await.unwrap().unwrap();
    assert_eq!(saga.state, SagaState::Compensated);
}

/// Projection durability gap on real Postgres: money fully settled (saga
/// `Settled`) but the margin row never landed. The reconciliation sweep, running
/// against the real anti-join query, backfills exactly that request's projection
/// from the saga's authorized event binding and moves no additional money.
#[tokio::test]
async fn pg_settled_but_unprojected_saga_is_reconciled() {
    let Some(store) = store_or_skip().await else {
        return;
    };
    let outbox: Arc<dyn OutboxStore> = Arc::new(PostgresOutbox::new(store.pool().clone()));
    let event = event_with_request_id(&unique_tag("req-D"));

    let mint = Arc::new(FakeMint::default());
    let coord = coordinator(mint.clone(), outbox.clone());

    // Settlement commits (money moved, PG saga Settled) but the ingest process
    // "crashes" before writing the margin projection.
    coord.settle(&event).await.expect("settle");
    assert_settled_once(&mint, &event);
    assert!(
        store
            .by_request_id(&event.request_id)
            .await
            .unwrap()
            .is_none(),
        "precondition: projection missing"
    );

    // Age the saga past the staleness window so the sweep treats it as stuck, and
    // confirm the real anti-join query surfaces it as unprojected.
    backdate(outbox.as_ref(), &event.request_id).await;
    let unprojected = outbox
        .list_unprojected_settled(Utc::now(), 1000)
        .await
        .unwrap();
    assert!(
        unprojected.iter().any(|s| s.request_id == event.request_id),
        "anti-join surfaces the settled-but-unprojected saga"
    );

    reconcile_projections(
        &store,
        outbox.as_ref(),
        &SystemClock,
        &RecoveryConfig::default(),
    )
    .await
    .expect("reconcile sweep");

    let record = store
        .by_request_id(&event.request_id)
        .await
        .unwrap()
        .expect("projection backfilled");
    assert_eq!(record.margin_micros, 300_000, "revenue 900k - cogs 600k");
    assert_eq!(record.event.request_id, event.request_id);
    assert!(record.settlement.cogs_sequence.is_some());
    // Reconciliation touches only the read model, never Mint.
    assert_settled_once(&mint, &event);

    // Now projected: the anti-join must no longer surface this request, and a
    // second sweep leaves the single row untouched.
    let after = outbox
        .list_unprojected_settled(Utc::now(), 1000)
        .await
        .unwrap();
    assert!(
        !after.iter().any(|s| s.request_id == event.request_id),
        "a projected saga is excluded from the anti-join"
    );
    reconcile_projections(
        &store,
        outbox.as_ref(),
        &SystemClock,
        &RecoveryConfig::default(),
    )
    .await
    .expect("second reconcile sweep");
    let record_again = store
        .by_request_id(&event.request_id)
        .await
        .unwrap()
        .expect("still exactly one projection");
    assert_eq!(record_again.created_at, record.created_at, "not re-written");
}

/// The anti-join is precise: a settled saga whose margin projection already
/// exists (the normal, no-crash path) is never surfaced for reconciliation.
#[tokio::test]
async fn pg_already_projected_saga_is_excluded_from_reconciliation() {
    let Some(store) = store_or_skip().await else {
        return;
    };
    let outbox: Arc<dyn OutboxStore> = Arc::new(PostgresOutbox::new(store.pool().clone()));
    let event = event_with_request_id(&unique_tag("req-E"));

    let mint = Arc::new(FakeMint::default());
    let coord = coordinator(mint.clone(), outbox.clone());

    // Full happy path: settle AND persist the projection.
    let refs = coord.settle(&event).await.expect("settle");
    let record = MarginRecord::new(event.clone(), refs, Utc::now()).expect("record");
    let (_, created) = store.save(&record).await.expect("save");
    assert!(created);
    backdate(outbox.as_ref(), &event.request_id).await;

    let unprojected = outbox
        .list_unprojected_settled(Utc::now(), 1000)
        .await
        .unwrap();
    assert!(
        !unprojected.iter().any(|s| s.request_id == event.request_id),
        "a fully projected settled saga is not a reconciliation candidate"
    );
}
