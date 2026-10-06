//! One driver per settlement saga, on the in-memory and the real Postgres
//! outbox (the Postgres half runs when `DATABASE_URL` is set).
//!
//! Before the lease, two instances (or an instance and the recovery sweep)
//! could drive one saga at once: the outbox write was last-writer-wins, so a
//! stalled driver overwrote newer progress, and a driver that saw an attempt
//! compensated could start the next attempt (fresh Mint keys) while an older
//! driver was still moving money on the previous one.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

use chrono::{DateTime, Duration, Utc};
use common::{assert_settled_once, event_with_request_id, unique_tag, FakeMint, Op};
use ledgerline::error::LedgerlineError;
use ledgerline::mint::MintSettlement;
use ledgerline::outbox::{MemoryOutbox, OutboxStore, PostgresOutbox, SagaState};
use ledgerline::settle::{Clock, SettlementCoordinator};
use ledgerline::store::PostgresStore;

/// A clock the test moves by hand.
struct ManualClock(Mutex<DateTime<Utc>>);

impl ManualClock {
    fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(Utc::now())))
    }
    fn advance(&self, by: Duration) {
        let mut t = self.0.lock().expect("lock");
        *t += by;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().expect("lock")
    }
}

async fn outboxes() -> Vec<(&'static str, Arc<dyn OutboxStore>)> {
    let mut out: Vec<(&'static str, Arc<dyn OutboxStore>)> =
        vec![("memory", Arc::new(MemoryOutbox::new()))];
    if let Ok(url) = std::env::var("DATABASE_URL") {
        let store = PostgresStore::connect(&url)
            .await
            .expect("connect to DATABASE_URL");
        store.migrate().await.expect("run migrations");
        out.push((
            "postgres",
            Arc::new(PostgresOutbox::new(store.pool().clone())),
        ));
    }
    out
}

async fn wait_for(mint: &FakeMint) {
    for _ in 0..500 {
        if mint.gated_hold_waiting() {
            return;
        }
        tokio::time::sleep(StdDuration::from_millis(5)).await;
    }
    panic!("the first driver never reached Mint");
}

#[tokio::test]
async fn a_second_driver_is_refused_while_the_first_holds_the_saga() {
    for (name, outbox) in outboxes().await {
        let mint = Arc::new(FakeMint::default());
        let clock = ManualClock::new();
        let a = SettlementCoordinator::new(mint.clone(), outbox.clone(), clock.clone());
        let b = SettlementCoordinator::new(mint.clone(), outbox.clone(), clock.clone());
        let event = event_with_request_id(&unique_tag("req-busy"));

        let gate = mint.gate_next_hold();
        let first = {
            let event = event.clone();
            tokio::spawn(async move { a.settle(&event).await })
        };
        wait_for(&mint).await;

        // The second instance (or a recovery sweep) moves nothing.
        assert_eq!(
            b.settle(&event).await.unwrap_err(),
            LedgerlineError::InProgress,
            "{name}"
        );
        assert_eq!(mint.effective_calls(Op::CreateHold), 0, "{name}");

        gate.notify_one();
        first.await.expect("join").expect("first driver settles");
        assert_settled_once(&mint, &event);
        assert_eq!(mint.effective_calls(Op::CreateHold), 1, "{name}");

        // With the lease given back, a replay is the idempotent answer.
        b.settle(&event).await.expect("replay after release");
        assert_settled_once(&mint, &event);
    }
}

#[tokio::test]
async fn a_driver_that_lost_its_lease_stops_and_cannot_overwrite_newer_progress() {
    for (name, outbox) in outboxes().await {
        let mint = Arc::new(FakeMint::default());
        let clock = ManualClock::new();
        let stalled = SettlementCoordinator::new(mint.clone(), outbox.clone(), clock.clone())
            .with_lease(Duration::seconds(10));
        let other = SettlementCoordinator::new(mint.clone(), outbox.clone(), clock.clone());
        let event = event_with_request_id(&unique_tag("req-takeover"));

        // Driver A stalls inside its first Mint call ...
        let gate = mint.gate_next_hold();
        let first = {
            let event = event.clone();
            tokio::spawn(async move { stalled.settle(&event).await })
        };
        wait_for(&mint).await;

        // ... past its lease: driver B takes the saga over and settles it.
        clock.advance(Duration::seconds(30));
        other.settle(&event).await.expect("takeover settles");
        assert_settled_once(&mint, &event);

        // A wakes up: its Mint call is the same keyed step (no new money), and
        // its next write is refused, so it stops without touching the saga.
        gate.notify_one();
        let late = first.await.expect("join");
        assert_eq!(late.unwrap_err(), LedgerlineError::InProgress, "{name}");
        let saga = outbox
            .load(&event.request_id)
            .await
            .expect("load")
            .expect("saga");
        assert_eq!(
            saga.state,
            SagaState::Settled,
            "{name}: not overwritten by the stalled driver"
        );
        assert_settled_once(&mint, &event);
        for op in [Op::CreateHold, Op::CaptureHold, Op::TransferCogs] {
            assert_eq!(mint.effective_calls(op), 1, "{name}: {op:?}");
        }
    }
}

#[tokio::test]
async fn many_concurrent_settles_of_one_request_move_money_once() {
    for (name, outbox) in outboxes().await {
        let mint = Arc::new(FakeMint::default());
        let clock = ManualClock::new();
        let event = event_with_request_id(&unique_tag("req-herd"));
        let mut tasks = Vec::new();
        for _ in 0..12 {
            let c = SettlementCoordinator::new(mint.clone(), outbox.clone(), clock.clone());
            let event = event.clone();
            tasks.push(tokio::spawn(async move { c.settle(&event).await }));
        }
        let mut settled = 0;
        for t in tasks {
            match t.await.expect("join") {
                Ok(_) => settled += 1,
                Err(LedgerlineError::InProgress) => {}
                Err(other) => panic!("{name}: {other}"),
            }
        }
        assert!(settled >= 1, "{name}");
        assert_settled_once(&mint, &event);
        for op in [Op::CreateHold, Op::CaptureHold, Op::TransferCogs] {
            assert_eq!(mint.effective_calls(op), 1, "{name}: {op:?}");
        }
    }
}
