//! Live-Postgres proof for the durable settlement outbox.
//!
//! These tests exercise the real SQL of the `PostgresSettlementCommandStore`
//! (idempotent upsert, state transitions, `claim_due` `FOR UPDATE SKIP LOCKED`
//! leasing, lease expiry, and idempotent terminal replay) against a running
//! Postgres 17 instance. They are gated on a database URL probe: when neither
//! `RELAY_TEST_DATABASE_URL` nor `DATABASE_URL` is set (e.g. DB-less CI) every
//! test short-circuits and is reported as passing without touching a database.
//!
//! Run locally, from the workspace root:
//! ```bash
//! RELAY_TEST_DATABASE_URL=postgres://user:pw@localhost:5432/db \
//!     cargo test -p relay-storage-postgres
//! ```
//!
//! `claim_due` scans the whole table, so these tests must not race each other
//! for due rows. They serialize on a process-wide async mutex; run them under
//! `cargo test` (single process), not a process-per-test runner.

use std::collections::HashSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use uuid::Uuid;

use relay_billing::mint::fake::FakeMint;
use relay_billing::{
    AccountId, BillingError, BillingLedger, HttpRequest, HttpResponse, HttpTransport, JobId,
    Micros, MintLedger, ModelId, Reservation, TransportError, Usage,
};
use relay_cost_feed::InferenceRoute;

use super::{
    BillingCommand, CommandState, DurableBillingCoordinator, PostgresSettlementCommandStore,
    ReservePayload, SettlementCommandStore, TerminalPayload,
};

/// Serializes every live-DB test in this binary so that the global,
/// owner-agnostic `claim_due` scan never observes another test's due rows.
static DB_TEST_GUARD: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn db_url() -> Option<String> {
    std::env::var("RELAY_TEST_DATABASE_URL")
        .ok()
        .or_else(|| std::env::var("DATABASE_URL").ok())
        .filter(|value| !value.trim().is_empty())
}

/// Connect and apply the embedded Relay migrations, or `None` when no database
/// URL is configured so DB-less CI skips instead of failing.
async fn connect_and_migrate() -> Option<PgPool> {
    let url = db_url()?;
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(&url)
        .await
        .expect("connect to the live test Postgres");
    crate::migrate(&pool)
        .await
        .expect("apply the embedded Relay migrations");
    Some(pool)
}

macro_rules! require_pool {
    () => {
        match connect_and_migrate().await {
            Some(pool) => pool,
            None => {
                eprintln!(
                    "skipping live-DB outbox test: set RELAY_TEST_DATABASE_URL or DATABASE_URL"
                );
                return;
            }
        }
    };
}

fn store(pool: &PgPool) -> PostgresSettlementCommandStore {
    PostgresSettlementCommandStore { pool: pool.clone() }
}

/// A fresh reserve command with a globally-unique owner, request id, and
/// idempotency key so parallel test binaries never collide on unique indexes.
fn fresh_command(owner: &AccountId) -> (JobId, Uuid, BillingCommand) {
    let job = JobId::new();
    let lease = Uuid::now_v7();
    let command = BillingCommand {
        owner_principal: owner.clone(),
        payer_account: AccountId::new(format!("payer-{}", Uuid::now_v7())),
        payee_account: None,
        request_id: job,
        estimate_micros: Micros::new(50),
        held_amount_micros: Micros::new(50),
        reserve_idem_key: format!("relay-test:{}", Uuid::now_v7()),
        reserve_payload: ReservePayload::Direct,
        hold_id: None,
        state: CommandState::ReservePending,
        terminal_idem_key: None,
        terminal_payload: None,
        lease_owner: Some(lease),
        batch_id: None,
        billing_model: None,
    };
    (job, lease, command)
}

fn unique_owner() -> AccountId {
    AccountId::new(format!("owner-{}", Uuid::now_v7()))
}

fn reservation_for(command: &BillingCommand, hold: &str) -> Reservation {
    Reservation {
        job_id: command.request_id,
        owner_principal: command.owner_principal.clone(),
        account_id: command.payer_account.clone(),
        estimate_micros: command.estimate_micros,
        held_amount_micros: command.held_amount_micros,
        backend_reference: Some(hold.to_string()),
        settlement_payee: command.payee_account.clone(),
        execution_lease: command.lease_owner,
    }
}

async fn state_of(pool: &PgPool, owner: &AccountId, job: JobId) -> Option<String> {
    sqlx::query_scalar::<_, String>(
        "SELECT state FROM relay_billing_outbox WHERE owner_principal=$1 AND request_id=$2",
    )
    .bind(owner.as_str())
    .bind(job.as_uuid())
    .fetch_optional(pool)
    .await
    .expect("read outbox state")
}

async fn row_count(pool: &PgPool, owner: &AccountId, job: JobId) -> i64 {
    sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*)::BIGINT FROM relay_billing_outbox \
         WHERE owner_principal=$1 AND request_id=$2",
    )
    .bind(owner.as_str())
    .bind(job.as_uuid())
    .fetch_one(pool)
    .await
    .expect("count outbox rows")
}

/// Remove every durable row this test owns. `claim_due` and `recover_once` scan
/// the whole table, so each test scopes its cleanup to its globally-unique owner
/// (safe on any database) to keep leftover due rows from bleeding across tests.
async fn cleanup(pool: &PgPool, owner: &AccountId) {
    for query in [
        "DELETE FROM relay_billing_outbox WHERE owner_principal=$1",
        "DELETE FROM relay_settlement_evidence WHERE principal=$1",
        "DELETE FROM relay_cost_events WHERE owner_principal=$1",
    ] {
        sqlx::query(query)
            .bind(owner.as_str())
            .execute(pool)
            .await
            .expect("clean up test-owned rows");
    }
}

/// Force every live row for `owner` to look like an expired/abandoned lease so a
/// recovery worker may re-claim it via `claim_due`.
async fn expire_leases(pool: &PgPool, owner: &AccountId) {
    sqlx::query(
        "UPDATE relay_billing_outbox SET lease_until=NOW()-INTERVAL '1 second', lease_owner=NULL \
         WHERE owner_principal=$1",
    )
    .bind(owner.as_str())
    .execute(pool)
    .await
    .expect("expire leases");
}

// ---------------------------------------------------------------------------
// Invariant 1: idempotent upsert / begin_reserve
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pg_begin_reserve_is_idempotent_and_single_row() {
    let _serial = DB_TEST_GUARD.lock().await;
    let pool = require_pool!();
    let store = store(&pool);
    let owner = unique_owner();
    let (job, _lease, command) = fresh_command(&owner);

    let first = store.begin_reserve(&command).await.expect("first reserve");
    let second = store
        .begin_reserve(&command)
        .await
        .expect("duplicate reserve must not error");

    assert!(first.created, "the first reserve creates the durable row");
    assert!(
        !second.created,
        "the same settlement key must never create a second row"
    );
    assert_eq!(
        first.command.lease_owner, second.command.lease_owner,
        "an idempotent reserve returns the original execution owner"
    );
    assert_eq!(
        row_count(&pool, &owner, job).await,
        1,
        "same settlement id -> exactly one row"
    );
    cleanup(&pool, &owner).await;
}

#[tokio::test]
async fn pg_begin_reserve_rejects_conflicting_immutable_command() {
    let _serial = DB_TEST_GUARD.lock().await;
    let pool = require_pool!();
    let store = store(&pool);
    let owner = unique_owner();
    let (job, _lease, command) = fresh_command(&owner);

    store.begin_reserve(&command).await.expect("first reserve");

    let mut conflicting = command.clone();
    conflicting.estimate_micros = Micros::new(999);
    let rejected = matches!(
        store.begin_reserve(&conflicting).await,
        Err(BillingError::Store(_))
    );

    assert!(
        rejected,
        "a conflicting immutable reserve for the same settlement key must be rejected"
    );
    assert_eq!(
        row_count(&pool, &owner, job).await,
        1,
        "a rejected conflicting reserve must not create a second row"
    );
    cleanup(&pool, &owner).await;
}

// ---------------------------------------------------------------------------
// Invariant 2: state transitions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pg_state_transitions_drive_lifecycle() {
    let _serial = DB_TEST_GUARD.lock().await;
    let pool = require_pool!();
    let store = store(&pool);
    let owner = unique_owner();
    let (job, _lease, command) = fresh_command(&owner);

    store.begin_reserve(&command).await.expect("reserve");
    assert_eq!(
        state_of(&pool, &owner, job).await.as_deref(),
        Some("reserve_pending")
    );

    let reservation = reservation_for(&command, "hold_lifecycle");
    store.mark_held(&reservation).await.expect("mark held");
    assert_eq!(state_of(&pool, &owner, job).await.as_deref(), Some("held"));

    store
        .mark_inference_started(&reservation)
        .await
        .expect("mark inference started");
    assert_eq!(
        state_of(&pool, &owner, job).await.as_deref(),
        Some("inference_started")
    );

    let staged = store
        .stage_terminal(&reservation, &TerminalPayload::Release)
        .await
        .expect("stage terminal release");
    assert!(
        matches!(staged.state, CommandState::TerminalPending),
        "staging a terminal command drives the row to terminal_pending"
    );
    assert_eq!(
        state_of(&pool, &owner, job).await.as_deref(),
        Some("terminal_pending")
    );
    cleanup(&pool, &owner).await;
}

#[tokio::test]
async fn pg_illegal_transitions_are_rejected() {
    let _serial = DB_TEST_GUARD.lock().await;
    let pool = require_pool!();
    let store = store(&pool);
    let owner = unique_owner();
    let (_job, _lease, command) = fresh_command(&owner);

    store.begin_reserve(&command).await.expect("reserve");
    let reservation = reservation_for(&command, "hold_illegal");

    // Out-of-order: inference cannot start before the hold is durable.
    assert!(
        store.mark_inference_started(&reservation).await.is_err(),
        "inference must not start before the reservation is held"
    );

    // A stale worker (wrong fencing token) cannot advance the row.
    let mut stale = reservation.clone();
    stale.execution_lease = Some(Uuid::now_v7());
    assert!(
        store.mark_held(&stale).await.is_err(),
        "a stale lease owner must not mark the hold"
    );

    // The legitimate worker advances held -> terminal_pending, and the terminal
    // direction is then immutable.
    store.mark_held(&reservation).await.expect("mark held");
    store
        .stage_terminal(&reservation, &TerminalPayload::Release)
        .await
        .expect("stage release");
    let capture = TerminalPayload::Capture {
        usage: Usage::new(1, 1, 2, Micros::new(4)),
        model: ModelId::new("tensorzero::model_name::test"),
        route: InferenceRoute::Unknown,
    };
    assert!(
        store.stage_terminal(&reservation, &capture).await.is_err(),
        "capture/release is immutable once a terminal direction is selected"
    );
    cleanup(&pool, &owner).await;
}

// ---------------------------------------------------------------------------
// Invariant 3: claim_due lease contention (FOR UPDATE SKIP LOCKED)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pg_claim_due_leases_each_due_row_to_exactly_one_worker() {
    let _serial = DB_TEST_GUARD.lock().await;
    let pool = require_pool!();
    let store = store(&pool);
    let owner = unique_owner();

    // N due rows for this owner.
    let n: usize = 8;
    let mut due_jobs = HashSet::new();
    for _ in 0..n {
        let (job, _lease, command) = fresh_command(&owner);
        store
            .begin_reserve(&command)
            .await
            .expect("reserve due row");
        due_jobs.insert(job);
    }
    // begin_reserve leases each row for an hour; simulate a dead worker so the
    // rows are eligible for recovery.
    expire_leases(&pool, &owner).await;

    // One not-yet-due row that must never be claimed.
    let (future_job, _fl, future_command) = fresh_command(&owner);
    store
        .begin_reserve(&future_command)
        .await
        .expect("reserve future row");
    sqlx::query(
        "UPDATE relay_billing_outbox SET available_at=NOW()+INTERVAL '1 hour', \
         lease_until=NULL, lease_owner=NULL WHERE owner_principal=$1 AND request_id=$2",
    )
    .bind(owner.as_str())
    .bind(future_job.as_uuid())
    .execute(&pool)
    .await
    .expect("defer future row");

    let worker_a = Uuid::now_v7();
    let worker_b = Uuid::now_v7();
    let store_a = store.clone();
    let store_b = store.clone();
    let (claimed_a, claimed_b) = tokio::join!(
        async move { store_a.claim_due(worker_a, 100).await },
        async move { store_b.claim_due(worker_b, 100).await },
    );
    let claimed_a = claimed_a.expect("worker a claim_due");
    let claimed_b = claimed_b.expect("worker b claim_due");

    let mine = |commands: &[BillingCommand], worker: Uuid| -> HashSet<JobId> {
        for command in commands.iter().filter(|c| c.owner_principal == owner) {
            assert_eq!(
                command.lease_owner,
                Some(worker),
                "a claimed row must be leased to its claiming worker"
            );
        }
        commands
            .iter()
            .filter(|c| c.owner_principal == owner)
            .map(|c| c.request_id)
            .collect()
    };
    let a_ids = mine(&claimed_a, worker_a);
    let b_ids = mine(&claimed_b, worker_b);

    assert!(
        a_ids.is_disjoint(&b_ids),
        "SKIP LOCKED must never lease one row to two workers: A={a_ids:?} B={b_ids:?}"
    );
    let union: HashSet<JobId> = a_ids.union(&b_ids).copied().collect();
    assert_eq!(
        union, due_jobs,
        "every due row is leased exactly once across the two workers"
    );
    assert!(
        !union.contains(&future_job),
        "a not-yet-due row must not be claimed"
    );
    cleanup(&pool, &owner).await;
}

#[tokio::test]
async fn pg_expired_lease_is_reclaimable_but_live_lease_is_not() {
    let _serial = DB_TEST_GUARD.lock().await;
    let pool = require_pool!();
    let store = store(&pool);
    let owner = unique_owner();
    let (job, _lease, command) = fresh_command(&owner);
    store.begin_reserve(&command).await.expect("reserve");

    // The active reserve lease has not expired: recovery must skip it.
    let skip_worker = Uuid::now_v7();
    let skipped = store.claim_due(skip_worker, 10).await.expect("claim live");
    assert!(
        skipped.iter().all(|c| c.owner_principal != owner),
        "a live (unexpired) lease must not be claimed by recovery"
    );

    // Once the lease expires it becomes claimable exactly once...
    expire_leases(&pool, &owner).await;
    let worker_one = Uuid::now_v7();
    let first = store
        .claim_due(worker_one, 10)
        .await
        .expect("claim expired");
    let mine_first: Vec<_> = first
        .iter()
        .filter(|c| c.owner_principal == owner)
        .collect();
    assert_eq!(mine_first.len(), 1, "an expired lease is re-claimable");
    assert_eq!(mine_first[0].request_id, job);
    assert_eq!(mine_first[0].lease_owner, Some(worker_one));

    // ...and claiming renews the lease (NOW()+30s), so an immediate second pass
    // sees it as live and does not re-claim.
    let worker_two = Uuid::now_v7();
    let second = store
        .claim_due(worker_two, 10)
        .await
        .expect("claim renewed");
    assert!(
        second.iter().all(|c| c.owner_principal != owner),
        "a freshly-renewed lease must not be double-claimed"
    );

    // Expire again -> re-claimable by a new worker.
    expire_leases(&pool, &owner).await;
    let third = store
        .claim_due(worker_two, 10)
        .await
        .expect("claim re-expired");
    let mine_third: Vec<_> = third
        .iter()
        .filter(|c| c.owner_principal == owner)
        .collect();
    assert_eq!(mine_third.len(), 1, "a re-expired lease is re-claimable");
    assert_eq!(mine_third[0].lease_owner, Some(worker_two));
    cleanup(&pool, &owner).await;
}

// ---------------------------------------------------------------------------
// Invariant 4: idempotent terminal replay / complete (full coordinator path)
// ---------------------------------------------------------------------------

/// Mint transport that fails the first `/capture` and records every request so
/// the test can assert the replay reuses the identical wire command.
struct FailFirstCaptureTransport {
    inner: FakeMint,
    capture_failures_remaining: AtomicUsize,
    requests: Mutex<Vec<HttpRequest>>,
}

impl FailFirstCaptureTransport {
    fn new() -> Self {
        Self {
            inner: FakeMint::funded(1_000).with_hold_id("hold_pg_replay"),
            capture_failures_remaining: AtomicUsize::new(1),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn matching(&self, suffix: &str) -> Vec<HttpRequest> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .filter(|request| request.path.ends_with(suffix))
            .cloned()
            .collect()
    }

    fn captures(&self) -> Vec<HttpRequest> {
        self.matching("/capture")
    }

    fn releases(&self) -> Vec<HttpRequest> {
        self.matching("/release")
    }
}

#[async_trait]
impl HttpTransport for FailFirstCaptureTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.requests
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(request.clone());
        if request.path.ends_with("/capture")
            && self
                .capture_failures_remaining
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                    remaining.checked_sub(1)
                })
                .is_ok()
        {
            return Err(TransportError("injected Mint capture outage".into()));
        }
        self.inner.send(request).await
    }
}

#[tokio::test]
async fn pg_terminal_replay_completes_exactly_once() {
    let _serial = DB_TEST_GUARD.lock().await;
    let pool = require_pool!();

    let transport = Arc::new(FailFirstCaptureTransport::new());
    let ledger: Arc<dyn BillingLedger> = Arc::new(MintLedger::new(
        transport.clone(),
        "mint-test-key",
        "acct_payee",
        None,
    ));
    let coordinator = DurableBillingCoordinator::postgres(ledger, pool.clone());

    let payer = AccountId::new(format!("acct_payer-{}", Uuid::now_v7()));
    let job = JobId::new();
    let reservation = coordinator
        .reserve(&payer, Micros::new(50), job)
        .await
        .expect("durable reserve");
    coordinator
        .mark_inference_started(&reservation)
        .await
        .expect("mark inference started");

    // First terminal capture is injected to fail: the row stays terminal_pending.
    coordinator
        .settle(
            &reservation,
            Usage::new(2, 3, 5, Micros::new(4)),
            ModelId::new("tensorzero::model_name::test"),
            InferenceRoute::Unknown,
        )
        .await
        .expect_err("the first Mint capture is intentionally unavailable");
    assert_eq!(
        state_of(&pool, &reservation.owner_principal, job)
            .await
            .as_deref(),
        Some("terminal_pending"),
        "a failed capture must leave the immutable terminal direction staged"
    );

    // retry() defers the row 5s; simulate that elapsing so recovery can claim it.
    sqlx::query(
        "UPDATE relay_billing_outbox SET available_at=NOW() \
         WHERE owner_principal=$1 AND request_id=$2",
    )
    .bind(reservation.owner_principal.as_str())
    .bind(job.as_uuid())
    .execute(&pool)
    .await
    .expect("make the stalled terminal command due");

    assert_eq!(
        coordinator.recover_once(10).await.expect("recovery pass"),
        1,
        "the stalled terminal command is claimed and replayed once"
    );
    assert_eq!(
        coordinator
            .recover_once(10)
            .await
            .expect("second recovery pass"),
        0,
        "a completed command is never replayed again"
    );

    let captures = transport.captures();
    assert_eq!(
        captures.len(),
        2,
        "exactly one failed capture plus one idempotent replay"
    );
    assert_eq!(
        captures[0].path, captures[1].path,
        "the replay targets the identical hold capture endpoint"
    );
    assert_eq!(
        captures[0].body, captures[1].body,
        "the replay reuses the identical wire command (idempotency key + amount)"
    );
    assert!(
        transport.releases().is_empty(),
        "a captured settlement must never become a free release"
    );
    assert_eq!(
        state_of(&pool, &reservation.owner_principal, job)
            .await
            .as_deref(),
        Some("completed"),
        "the durable row reaches the terminal completed state"
    );
    cleanup(&pool, &reservation.owner_principal).await;
}
