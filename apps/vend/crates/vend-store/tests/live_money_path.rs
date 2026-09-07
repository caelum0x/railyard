//! Live-Postgres proof of Vend's real money-path SQL.
//!
//! Unlike `vend-server/tests/metered_flow.rs` (which drives the `InMemoryStore`
//! analog) these tests run the ACTUAL `VendStore` SQL against a real Postgres so
//! the atomic cap gate (`pg_advisory_xact_lock`), the idempotency claim/abort
//! race, and the `SKIP LOCKED` lease CTEs are proven on the same statements that
//! run in production — closing the analog↔SQL divergence gap.
//!
//! Gated: these tests are skipped (no-op pass) unless a Postgres URL is provided
//! via `VEND_TEST_DATABASE_URL`, `VEND_DATABASE_URL`, or `DATABASE_URL`, so a
//! DB-less CI run stays green.
//!
//! Each test is data-isolated on a shared database:
//! - cap tests scope by a fresh `SubKeyId` (the cap SUM filters by `sub_key_id`),
//! - idempotency tests use fresh keys,
//! - the lease test back-dates its rows to the year 2000 and leases with an
//!   `older_than` cutoff of 2001, so no other test's `now()`-stamped rows are
//!   ever candidates.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use sqlx::postgres::PgPoolOptions;

use vend_core::ids::{AppId, ChargeId, SubKeyId, TenantId};
use vend_core::payout::{PayoutExecution, PayoutRepo, PayoutState};
use vend_core::ports::{IdempotencyClaim, IdempotencyRepo, MeteredUsage, UsageOutcome, UsageRepo};
use vend_store::VendStore;

fn database_url() -> Option<String> {
    for var in [
        "VEND_TEST_DATABASE_URL",
        "VEND_DATABASE_URL",
        "DATABASE_URL",
    ] {
        if let Ok(value) = std::env::var(var) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// Connect a pool sized for the test's concurrency and apply migrations.
/// `migrate` is advisory-lock guarded, so concurrent test binaries are safe.
async fn connect_and_migrate(max_connections: u32) -> VendStore {
    let url = database_url().expect("database url present (checked by caller)");
    let pool = PgPoolOptions::new()
        .max_connections(max_connections)
        .acquire_timeout(Duration::from_secs(30))
        .connect(&url)
        .await
        .expect("connect to test Postgres");
    let store = VendStore::from_pool(pool);
    store.migrate().await.expect("run migrations");
    store
}

/// Build a fresh pending usage row for a given key, priced at `charge` µ$.
fn usage_for(sub_key_id: &SubKeyId, charge: i64, utc_day: &str) -> MeteredUsage {
    MeteredUsage {
        charge_id: ChargeId::generate(),
        sub_key_id: sub_key_id.clone(),
        tenant_id: TenantId::generate(),
        app_id: AppId::generate(),
        end_user_id: None,
        idem_key: format!("vend:{}", ChargeId::generate()),
        hold_id: "cap-test-hold".to_string(),
        provider_cost_micros: 0,
        charge_micros: charge,
        platform_fee_micros: 0,
        tenant_net_micros: 0,
        outcome: UsageOutcome::Pending,
        settlement_proof: None,
        utc_day: utc_day.to_string(),
    }
}

// --------------------------------------------------------------------------
// INVARIANT 1: atomic cap enforcement under concurrency.
//
// N concurrent reservations against a cap of exactly K units must let EXACTLY K
// through and reject N-K, and the persisted SUM must never exceed the cap. This
// exercises the real `pg_advisory_xact_lock` + re-check-in-tx gate — a TOCTOU
// bug or a missing lock would let two readers both slip under the cap.
// --------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_reservations_never_exceed_daily_cap() {
    if database_url().is_none() {
        eprintln!("skipping concurrent_reservations_never_exceed_daily_cap: no test DB url");
        return;
    }

    const UNIT: i64 = 1_000;
    const K: i64 = 8; // capacity, in units
    const N: usize = 24; // concurrent attempts
    let cap = K * UNIT;

    let store = Arc::new(connect_and_migrate(16).await);
    let sub_key_id = SubKeyId::generate();
    let utc_day = Utc::now().format("%Y-%m-%d").to_string();

    let mut handles = Vec::with_capacity(N);
    for _ in 0..N {
        let store = store.clone();
        let sub_key_id = sub_key_id.clone();
        let utc_day = utc_day.clone();
        handles.push(tokio::spawn(async move {
            let usage = usage_for(&sub_key_id, UNIT, &utc_day);
            store
                .record_pending_within_cap(&usage, Some(cap), None)
                .await
        }));
    }

    let mut accepted = 0i64;
    let mut rejected = 0usize;
    for handle in handles {
        match handle.await.expect("task join") {
            Ok(()) => accepted += 1,
            Err(vend_core::VendError::SpendCapExceeded) => rejected += 1,
            Err(other) => panic!("unexpected error from cap gate: {other:?}"),
        }
    }

    assert_eq!(
        accepted, K,
        "exactly K reservations must succeed under the advisory-lock cap gate"
    );
    assert_eq!(
        rejected,
        N - K as usize,
        "the remaining N-K must be rejected"
    );

    // Ground truth from the DB: persisted spend must equal K*UNIT and never
    // exceed the cap. This is the real SUM the gate re-evaluates.
    let spent = store
        .sum_charge_for_key_day(&sub_key_id, &utc_day)
        .await
        .expect("sum charge");
    assert_eq!(spent, K * UNIT, "persisted spend equals K units");
    assert!(spent <= cap, "persisted spend never exceeds the cap");
}

// --------------------------------------------------------------------------
// INVARIANT 2a: concurrent idempotency claim — exactly one winner.
//
// Two concurrent claims of the same key/hash: exactly one must `Acquired`, and
// the loser must observe a RETRIABLE outcome (`InProgress`) — never a spurious
// non-retriable `Storage` error and never a second `Acquired` (double-charge).
// --------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_claim_yields_exactly_one_acquired() {
    if database_url().is_none() {
        eprintln!("skipping concurrent_claim_yields_exactly_one_acquired: no test DB url");
        return;
    }

    let store = Arc::new(connect_and_migrate(8).await);
    let key = format!("idem-one-winner-{}", ChargeId::generate());
    let hash = "request-hash-A";

    let mut handles = Vec::new();
    for _ in 0..12 {
        let store = store.clone();
        let key = key.clone();
        handles.push(tokio::spawn(async move { store.claim(&key, hash).await }));
    }

    let mut acquired = 0;
    let mut in_progress = 0;
    for handle in handles {
        match handle
            .await
            .expect("task join")
            .expect("claim must not error")
        {
            IdempotencyClaim::Acquired => acquired += 1,
            IdempotencyClaim::InProgress => in_progress += 1,
            other => panic!("unexpected claim outcome for identical request: {other:?}"),
        }
    }

    assert_eq!(acquired, 1, "exactly one caller may acquire the key");
    assert_eq!(
        in_progress, 11,
        "every loser observes the in-progress claim (retriable), not an error"
    );
}

// --------------------------------------------------------------------------
// INVARIANT 2b: claim vs. concurrent abort must never surface RowNotFound.
//
// This is where the audit's MEDIUM lives: `claim` does `INSERT .. ON CONFLICT
// DO NOTHING` then a SEPARATE `SELECT .. fetch_one`. If a concurrent `abort`
// deletes the pending row between those two statements, `fetch_one` raises
// `RowNotFound`, surfaced as a NON-retriable `Storage` error. Two workers race
// claim+abort on the same key many times; the invariant is that no claim ever
// returns such a spurious storage error.
// --------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn claim_racing_abort_never_returns_row_not_found() {
    if database_url().is_none() {
        eprintln!("skipping claim_racing_abort_never_returns_row_not_found: no test DB url");
        return;
    }

    let store = Arc::new(connect_and_migrate(8).await);
    let key = format!("idem-race-{}", ChargeId::generate());
    let hash = "race-hash";
    const ITERS: usize = 1_500;

    // Each worker repeatedly claims; if it won, it aborts (freeing the key). The
    // interleaving `claim(conflict-insert) → other.abort(delete) → claim.select`
    // is exactly the window that trips fetch_one's RowNotFound.
    async fn worker(store: Arc<VendStore>, key: String, hash: &'static str) -> Result<(), String> {
        for _ in 0..ITERS {
            match store.claim(&key, hash).await {
                Ok(IdempotencyClaim::Acquired) => {
                    // Free the key for the next round; ignore benign races.
                    let _ = store.abort(&key, hash).await;
                }
                Ok(_) => {}
                Err(vend_core::VendError::Storage(msg)) if msg.contains("no rows") => {
                    return Err(format!("spurious RowNotFound from claim: {msg}"));
                }
                Err(other) => return Err(format!("unexpected claim error: {other:?}")),
            }
        }
        Ok(())
    }

    let a = tokio::spawn(worker(store.clone(), key.clone(), hash));
    let b = tokio::spawn(worker(store.clone(), key.clone(), hash));
    let ra = a.await.expect("join a");
    let rb = b.await.expect("join b");
    assert!(ra.is_ok(), "worker A: {ra:?}");
    assert!(rb.is_ok(), "worker B: {rb:?}");
}

// --------------------------------------------------------------------------
// INVARIANT 3: SKIP LOCKED lease never double-dispatches a row.
//
// Two workers lease the same pool of pending, unattached holds concurrently.
// `FOR UPDATE OF u SKIP LOCKED` must partition the rows so no charge_id is
// handed to both workers.
// --------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_leases_never_dispatch_a_row_twice() {
    if database_url().is_none() {
        eprintln!("skipping concurrent_leases_never_dispatch_a_row_twice: no test DB url");
        return;
    }

    let store = Arc::new(connect_and_migrate(8).await);

    // A dedicated tenant + app so the lease's JOIN apps resolves. Unique ids keep
    // this test's rows disjoint from every other test.
    let tenant_id = TenantId::generate();
    let app_id = AppId::generate();
    sqlx::query("INSERT INTO tenants (id, name, mint_account_id) VALUES ($1, $2, $3)")
        .bind(tenant_id.as_str())
        .bind("lease-test-tenant")
        .bind("acct_tenant")
        .execute(store.pool())
        .await
        .expect("seed tenant");
    sqlx::query("INSERT INTO apps (id, tenant_id, name, mint_account_id) VALUES ($1, $2, $3, $4)")
        .bind(app_id.as_str())
        .bind(tenant_id.as_str())
        .bind("lease-test-app")
        .bind("acct_app_payer")
        .execute(store.pool())
        .await
        .expect("seed app");

    // Back-date the rows to the year 2000 so only THIS test (leasing with an
    // older_than of 2001) can select them.
    let backdated = Utc.with_ymd_and_hms(2000, 1, 1, 0, 0, 0).unwrap();
    let older_than = Utc.with_ymd_and_hms(2001, 1, 1, 0, 0, 0).unwrap();
    const ROWS: usize = 12;
    let mut seeded: HashSet<String> = HashSet::new();
    for _ in 0..ROWS {
        let charge_id = ChargeId::generate();
        seeded.insert(charge_id.as_str().to_string());
        sqlx::query(
            "INSERT INTO metered_usage (charge_id, sub_key_id, tenant_id, app_id, end_user_id, \
             idem_key, hold_id, provider_cost_micros, charge_micros, platform_fee_micros, \
             tenant_net_micros, outcome, utc_day, created_at) \
             VALUES ($1,$2,$3,$4,NULL,$5,'',0,1000,0,0,'pending','2000-01-01',$6)",
        )
        .bind(charge_id.as_str())
        .bind(SubKeyId::generate().as_str())
        .bind(tenant_id.as_str())
        .bind(app_id.as_str())
        .bind(format!("vend:{}", ChargeId::generate()))
        .bind(backdated)
        .execute(store.pool())
        .await
        .expect("seed pending unattached hold");
    }

    // Two workers lease concurrently, each willing to take the whole batch.
    let store_a = store.clone();
    let store_b = store.clone();
    let (a, b) = tokio::join!(
        tokio::spawn(async move {
            store_a
                .lease_unattached_holds(older_than, ROWS as i64, 120, "worker-a")
                .await
        }),
        tokio::spawn(async move {
            store_b
                .lease_unattached_holds(older_than, ROWS as i64, 120, "worker-b")
                .await
        }),
    );
    let leased_a = a.expect("join a").expect("lease a");
    let leased_b = b.expect("join b").expect("lease b");

    let set_a: HashSet<String> = leased_a
        .iter()
        .map(|h| h.charge_id.as_str().to_string())
        .collect();
    let set_b: HashSet<String> = leased_b
        .iter()
        .map(|h| h.charge_id.as_str().to_string())
        .collect();

    // No row may be dispatched to both workers.
    let overlap: Vec<&String> = set_a.intersection(&set_b).collect();
    assert!(
        overlap.is_empty(),
        "SKIP LOCKED must never hand the same row to two workers; overlap: {overlap:?}"
    );
    // No duplicate rows within a single worker's batch.
    assert_eq!(
        set_a.len(),
        leased_a.len(),
        "worker A batch has no duplicates"
    );
    assert_eq!(
        set_b.len(),
        leased_b.len(),
        "worker B batch has no duplicates"
    );
    // Every leased row is one we seeded (isolation held).
    for id in set_a.iter().chain(set_b.iter()) {
        assert!(
            seeded.contains(id),
            "leased a row this test did not seed: {id}"
        );
    }
}

// --------------------------------------------------------------------------
// INVARIANT 4: claim → abort → re-claim frees the key (no stuck state).
// Plus claim → complete → claim replays the stored terminal response.
// --------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn abort_frees_key_for_fresh_claim_and_complete_replays() {
    if database_url().is_none() {
        eprintln!("skipping abort_frees_key_for_fresh_claim_and_complete_replays: no test DB url");
        return;
    }

    let store = connect_and_migrate(4).await;
    let key = format!("idem-lifecycle-{}", ChargeId::generate());
    let hash = "lifecycle-hash";

    // First claim acquires.
    assert_eq!(
        store.claim(&key, hash).await.expect("claim 1"),
        IdempotencyClaim::Acquired
    );
    // A second claim while pending is in-progress (retriable).
    assert_eq!(
        store.claim(&key, hash).await.expect("claim while pending"),
        IdempotencyClaim::InProgress
    );
    // Abort releases the pending claim.
    store.abort(&key, hash).await.expect("abort");
    // Re-claim must succeed cleanly — no stuck state.
    assert_eq!(
        store.claim(&key, hash).await.expect("re-claim after abort"),
        IdempotencyClaim::Acquired,
        "an aborted key must be freshly acquirable"
    );
    // Complete then re-claim replays the stored terminal body verbatim.
    let body = r#"{"choices":[]}"#;
    store.complete(&key, hash, body).await.expect("complete");
    assert_eq!(
        store.claim(&key, hash).await.expect("claim after complete"),
        IdempotencyClaim::Replay(body.to_string()),
        "a completed claim replays its stored response"
    );
}

// --------------------------------------------------------------------------
// INVARIANT 5: crash convergence of an ATTACHED hold against the real SQL.
//
// A process that opened a Mint hold, wrote a `pending` `metered_usage` row and a
// `pending` client idempotency claim, then crashed mid-stream is exactly the
// case the hold-reaper / reconciler recovers. This proves the two production
// convergence statements — `finish_attached_release` and `finish_attached_capture`
// — on real Postgres, closing the gap where only the pure decision helper
// (`plan_attached_reap`) and the lease CTE were covered:
//
//  - Mint `Open` (no payout leg) → release: the projection becomes `released`
//    AND the stranded client key is FREED so the caller can safely retry.
//  - Mint `Captured` → restore: the exact rated charge is rebuilt from the
//    immutable payout plan (never re-derived) AND the stranded client key is
//    CONSUMED with a deterministic recovery replay (inference bytes are gone).
//
// Isolation: both rows are back-dated to the year 2000 and leased with an
// `older_than` cutoff of 2001, so no other test's `now()`-stamped attached rows
// (e.g. the cap test's `cap-test-hold`) are ever candidates, and this is the
// only test that leases the attached partition.
// --------------------------------------------------------------------------

/// Seed a back-dated `pending` usage row that already references a Mint hold.
/// `charge_micros` is intentionally the *unrated* value so a capture-restore can
/// be proven to overwrite it from the immutable payout plan.
async fn insert_attached_pending(
    store: &VendStore,
    charge_id: &ChargeId,
    idem_key: &str,
    hold_id: &str,
    charge_micros: i64,
    created_at: DateTime<Utc>,
) {
    sqlx::query(
        "INSERT INTO metered_usage (charge_id, sub_key_id, tenant_id, app_id, end_user_id, \
         idem_key, hold_id, provider_cost_micros, charge_micros, platform_fee_micros, \
         tenant_net_micros, outcome, utc_day, created_at) \
         VALUES ($1,$2,$3,$4,NULL,$5,$6,0,$7,0,0,'pending','2000-01-01',$8)",
    )
    .bind(charge_id.as_str())
    .bind(SubKeyId::generate().as_str())
    .bind(TenantId::generate().as_str())
    .bind(AppId::generate().as_str())
    .bind(idem_key)
    .bind(hold_id)
    .bind(charge_micros)
    .bind(created_at)
    .execute(store.pool())
    .await
    .expect("seed attached pending row");
}

async fn outcome_of(store: &VendStore, charge_id: &ChargeId) -> String {
    sqlx::query_scalar::<_, String>("SELECT outcome FROM metered_usage WHERE charge_id = $1")
        .bind(charge_id.as_str())
        .fetch_one(store.pool())
        .await
        .expect("read outcome")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attached_hold_reconcile_converges_release_and_capture() {
    if database_url().is_none() {
        eprintln!("skipping attached_hold_reconcile_converges_release_and_capture: no test DB url");
        return;
    }

    let store = connect_and_migrate(4).await;
    let worker = format!("reap-worker-{}", ChargeId::generate());
    let backdated = Utc.with_ymd_and_hms(2000, 1, 1, 0, 0, 0).unwrap();
    let older_than = Utc.with_ymd_and_hms(2001, 1, 1, 0, 0, 0).unwrap();
    let hash = "attached-reconcile-hash";

    // (1) An OPEN orphaned hold with a stranded pending client claim.
    let open_charge = ChargeId::generate();
    let open_suffix = format!("reap-open-{}", ChargeId::generate());
    let open_idem = format!("vend:{open_suffix}");
    assert_eq!(
        store
            .claim(&open_suffix, hash)
            .await
            .expect("claim open key"),
        IdempotencyClaim::Acquired
    );
    insert_attached_pending(
        &store,
        &open_charge,
        &open_idem,
        "mint-open-hold",
        5_000,
        backdated,
    )
    .await;

    // (2) A CAPTURED orphaned hold: unrated usage row + immutable payout plan +
    // stranded pending client claim (the mid-stream-crash-after-capture case).
    let cap_charge = ChargeId::generate();
    let cap_suffix = format!("reap-cap-{}", ChargeId::generate());
    let cap_idem = format!("vend:{cap_suffix}");
    assert_eq!(
        store.claim(&cap_suffix, hash).await.expect("claim cap key"),
        IdempotencyClaim::Acquired
    );
    insert_attached_pending(
        &store,
        &cap_charge,
        &cap_idem,
        "mint-captured-hold",
        0,
        backdated,
    )
    .await;
    let plan = PayoutExecution {
        charge_id: cap_charge.clone(),
        idem_key: cap_idem.clone(),
        clearing_account_id: "acct_clearing".into(),
        provider_account_id: "acct_provider".into(),
        tenant_account_id: "acct_tenant".into(),
        platform_account_id: "acct_platform".into(),
        provider_cost_micros: 700,
        tenant_net_micros: 240,
        platform_fee_micros: 60,
        captured_charge_micros: 1_000,
        provider_transferred: false,
        tenant_transferred: false,
        platform_transferred: false,
        state: PayoutState::Pending,
        last_error: None,
    };
    store
        .prepare_payout(&plan)
        .await
        .expect("stage payout plan");

    // Lease the attached partition. Both back-dated rows are claimed by THIS
    // worker; the fail-closed payout signal is false (no transfer leg observed).
    let leased = store
        .lease_attached_holds(older_than, 100, 120, &worker)
        .await
        .expect("lease attached holds");
    let open = leased
        .iter()
        .find(|c| c.charge_id == open_charge)
        .expect("open hold leased");
    let captured = leased
        .iter()
        .find(|c| c.charge_id == cap_charge)
        .expect("captured hold leased");
    assert!(!open.payout_started, "open hold has no observed payout leg");
    assert!(
        !captured.payout_started,
        "staged-but-untransferred plan is not payout_started"
    );

    // RELEASE path: converge to released and FREE the stranded client key.
    store
        .finish_attached_release(&open_charge, &worker)
        .await
        .expect("finish attached release");
    assert_eq!(outcome_of(&store, &open_charge).await, "released");
    assert_eq!(
        store
            .claim(&open_suffix, hash)
            .await
            .expect("re-claim freed key"),
        IdempotencyClaim::Acquired,
        "a released hold must free its stranded client idempotency key for retry"
    );

    // CAPTURE-RESTORE path: rebuild the exact rated charge from the immutable
    // plan and CONSUME the stranded client key with a recovery replay.
    store
        .finish_attached_capture(&cap_charge, &worker)
        .await
        .expect("finish attached capture");
    assert_eq!(outcome_of(&store, &cap_charge).await, "captured");
    let restored = sqlx::query(
        "SELECT provider_cost_micros, charge_micros, platform_fee_micros, tenant_net_micros \
         FROM metered_usage WHERE charge_id = $1",
    )
    .bind(cap_charge.as_str())
    .fetch_one(store.pool())
    .await
    .expect("read restored usage");
    use sqlx::Row;
    assert_eq!(
        restored.try_get::<i64, _>("charge_micros").unwrap(),
        1_000,
        "the rated charge is restored from the immutable payout plan, not the unrated row"
    );
    assert_eq!(
        restored.try_get::<i64, _>("provider_cost_micros").unwrap(),
        700
    );
    assert_eq!(
        restored.try_get::<i64, _>("platform_fee_micros").unwrap(),
        60
    );
    assert_eq!(
        restored.try_get::<i64, _>("tenant_net_micros").unwrap(),
        240
    );
    match store
        .claim(&cap_suffix, hash)
        .await
        .expect("re-claim consumed key")
    {
        IdempotencyClaim::Replay(body) => assert!(
            body.contains("settled_response_unavailable"),
            "a restored capture consumes its client key with a deterministic recovery replay"
        ),
        other => panic!("captured key must replay a terminal recovery response, got {other:?}"),
    }
}
