//! Live-Postgres acceptance for Abacus's money path over REAL SQL.
//!
//! The audit flagged Abacus's Postgres hold-recovery (`FOR UPDATE SKIP LOCKED`
//! reaper) and receipt persistence as untested — covered only by in-memory fakes.
//! This suite closes that gap: it connects to a live Postgres, runs the real
//! embedded migrations, and drives the genuine `sqlx` queries in
//! [`abacus_proxy::postgres_hold_store`] and
//! [`abacus_proxy::postgres_product_store`]. Nothing is mocked except the Mint
//! money authority (the `LedgerFakeMint`, which enforces the balance/allowance
//! ceiling invariant) and the upstream provider.
//!
//! The postgres modules always compile, so no cargo feature is required; the
//! LIVE connection is gated at runtime on `ABACUS_DATABASE_URL` (falling back to
//! `DATABASE_URL`). When neither is set — e.g. DB-less CI — every test prints a
//! SKIP line and returns, so the suite stays green without a database. Mirrors
//! sibling `turnstile`'s `postgres_settlement_journal.rs` pattern.
//!
//! Run against a throwaway PG:
//! ```bash
//! ABACUS_DATABASE_URL=postgres://postgres:abacus@127.0.0.1:55440/abacus_test \
//!   cargo test -p abacus-proxy --test postgres_money_path
//! ```
//!
//! Isolation: the recovery reaper scans the WHOLE `abacus_open_holds` table, so a
//! test's sweep could otherwise claim another test's rows. Every test therefore
//! runs under a process-wide serialization lock and truncates the recovery tables
//! on entry, guaranteeing determinism even under a multi-threaded test runner.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use axum::http::{HeaderMap, HeaderValue};
use tokio::sync::{Mutex, MutexGuard};

use abacus_core::error::AbacusError;
use abacus_core::hold_store::{HoldReceiptContext, HoldRecord, HoldStore, ManualClock};
use abacus_core::ids::{AccountId, AgentId, Ait1Address, BudgetId, RunId};
use abacus_core::receipt::{MintReceipt, ReceiptOutcome, ReceiptStore};
use abacus_core::transport::{Hold, MintTransport, SettlementProof, Usage};
use abacus_proxy::handler::{run_flow, AGENT_HEADER, REQUEST_HEADER};
use abacus_proxy::hold_reaper::reap_expired_claimed;
use abacus_proxy::postgres_hold_store::PostgresHoldStore;
use abacus_proxy::postgres_product_store::PostgresProductStore;
use abacus_proxy::{AppState, PricingTable};
use abacus_testkit::{FakeBudgetRepo, LedgerFakeMint, ScriptedUpstream};

const TREASURY: &str = "acct_treasury01";

fn db_url() -> Option<String> {
    std::env::var("ABACUS_DATABASE_URL")
        .ok()
        .or_else(|| std::env::var("DATABASE_URL").ok())
}

/// Process-wide serialization lock so the globally-scanned recovery tables are
/// never raced across tests in the same binary.
static SERIAL: OnceLock<Mutex<()>> = OnceLock::new();

/// Two store handles over ONE shared pool, plus the serialization guard held for
/// the whole test.
struct Db {
    holds: PostgresHoldStore,
    products: PostgresProductStore,
    _guard: MutexGuard<'static, ()>,
}

/// Connect + migrate + truncate under the serial lock, or `None` to SKIP.
async fn db() -> Option<Db> {
    let url = db_url()?;
    let guard = SERIAL.get_or_init(|| Mutex::new(())).lock().await;
    // One shared pool underlies both stores: the product store owns it, the hold
    // store borrows a clone — so the reaper and the receipt writer transact against
    // the same Postgres, exactly as the single-process server wires them.
    let products = PostgresProductStore::connect(&url)
        .await
        .expect("connect to ABACUS_DATABASE_URL");
    let pool = products.pool();
    let holds = PostgresHoldStore::from_pool(pool.clone());
    holds.migrate().await.expect("run abacus migrations");
    // Start every test from an empty table set: the reaper scans globally.
    sqlx::query(
        "TRUNCATE abacus_open_holds, abacus_receipts, abacus_agent_policies RESTART IDENTITY",
    )
    .execute(&pool)
    .await
    .expect("truncate recovery tables");
    Some(Db {
        holds,
        products,
        _guard: guard,
    })
}

macro_rules! db_or_skip {
    ($name:literal) => {
        match db().await {
            Some(db) => db,
            None => {
                eprintln!(concat!("SKIP ", $name, ": set ABACUS_DATABASE_URL"));
                return;
            }
        }
    };
}

fn receipt_context(request_id: &str) -> HoldReceiptContext {
    HoldReceiptContext {
        agent_id: AgentId::generate(),
        run_id: RunId::generate(),
        budget_id: BudgetId::generate(),
        payer_account: TREASURY.to_string(),
        request_id: request_id.to_string(),
    }
}

fn hold_record(
    hold_id: &str,
    idem: &str,
    micros: i64,
    created_at_ms: i64,
    deadline_ms: Option<i64>,
    request_id: &str,
) -> HoldRecord {
    HoldRecord {
        hold_id: hold_id.to_string(),
        idem_key: idem.to_string(),
        reserved_micros: micros,
        created_at_ms,
        deadline_ms,
        receipt_context: Some(receipt_context(request_id)),
    }
}

/// Open a REAL hold on the fake Mint (locking `micros` of balance) AND track it in
/// the Postgres store with a controlled age, returning the Mint-minted hold id.
async fn open_and_track(
    mint: &LedgerFakeMint,
    store: &PostgresHoldStore,
    idem: &str,
    micros: i64,
    created_at_ms: i64,
    deadline_ms: Option<i64>,
    request_id: &str,
) -> String {
    let receipt = mint
        .create_hold(Hold {
            account_id: TREASURY.to_string(),
            amount_micros: micros,
            idem_key: idem.to_string(),
        })
        .await
        .expect("hold opens on mint");
    let record = hold_record(
        &receipt.hold_id,
        idem,
        micros,
        created_at_ms,
        deadline_ms,
        request_id,
    );
    store.create(record).await.expect("track hold in postgres");
    receipt.hold_id
}

fn captured_receipt(hold_id: &str, request_id: &str, reserved: i64, captured: i64) -> MintReceipt {
    MintReceipt {
        agent_id: AgentId::generate(),
        run_id: RunId::generate(),
        budget_id: BudgetId::generate(),
        payer_account: TREASURY.to_string(),
        request_id: request_id.to_string(),
        hold_id: hold_id.to_string(),
        reserved_micros: reserved,
        captured_micros: captured,
        balance_after_micros: 1_000_000 - captured,
        settlement_seq: 42,
        outcome: ReceiptOutcome::Captured,
        settlement_proof: SettlementProof {
            sequence: 42,
            operation: "hold.capture".into(),
            operation_hash: "op_hash".into(),
            receipt_hash: "receipt_hash".into(),
            signature: "signature".into(),
        },
    }
}

fn uniq(tag: &str) -> String {
    format!("{tag}_{}", ulid::Ulid::new())
}

// ---------------------------------------------------------------------------
// 1. Hold tracking-row lifecycle + idempotent replay (create → get → remove).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hold_record_lifecycle_and_idempotent_replay() {
    let db = db_or_skip!("hold_record_lifecycle_and_idempotent_replay");
    let hid = uniq("hold");
    let rec = hold_record(&hid, "idem_a", 4_000, 1_000, None, "req_crud");

    // create → get round-trips the row (incl. the jsonb receipt_context) exactly.
    db.holds.create(rec.clone()).await.unwrap();
    assert_eq!(db.holds.get(&hid).await.unwrap().as_ref(), Some(&rec));

    // Idempotent replay of the IDENTICAL record is a clean no-op Ok — same key,
    // no double effect.
    db.holds.create(rec.clone()).await.unwrap();
    assert_eq!(db.holds.list().await.unwrap(), vec![rec.clone()]);

    // A DIFFERING record for the same hold_id is a genuine conflict.
    let mut different = rec.clone();
    different.reserved_micros = 9_999;
    assert_eq!(
        db.holds.create(different).await.unwrap_err(),
        AbacusError::IdempotencyConflict
    );

    // remove returns the row once; a second remove is a clean None, never an error.
    assert_eq!(db.holds.remove(&hid).await.unwrap().as_ref(), Some(&rec));
    assert_eq!(db.holds.remove(&hid).await.unwrap(), None);
    assert!(db.holds.list().await.unwrap().is_empty());
}

// ---------------------------------------------------------------------------
// 2. Recovery reaper on REAL SQL: expired holds released, live holds untouched,
//    Released receipts persisted, money conserved.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn reaper_recovers_expired_holds_and_conserves_money() {
    let db = db_or_skip!("reaper_recovers_expired_holds_and_conserves_money");
    let mint = LedgerFakeMint::new().with_balance(TREASURY, 20_000);

    // Two orphans created long ago + one fresh live hold. now=50_000, ttl=30_000
    // ⇒ expiry cutoff is 20_000: the two olds (created ≤ 1_000) are orphaned,
    // `live` (created 40_000) stays comfortably in-window.
    let a = open_and_track(&mint, &db.holds, "idem_a", 2_000, 0, None, "req_a").await;
    let b = open_and_track(&mint, &db.holds, "idem_b", 3_000, 1_000, None, "req_b").await;
    let live = open_and_track(
        &mint,
        &db.holds,
        "idem_live",
        5_000,
        40_000,
        None,
        "req_live",
    )
    .await;
    assert_eq!(mint.balance(TREASURY), 10_000); // 20k - 2k - 3k - 5k

    let ttl = 30_000;
    let clock = ManualClock::new(50_000);
    let summary = reap_expired_claimed(
        &db.holds,
        &mint,
        &db.products,
        &clock,
        ttl,
        "worker-solo",
        50_000 + ttl,
        usize::MAX,
    )
    .await
    .unwrap();

    let mut reaped = summary.reaped_hold_ids.clone();
    reaped.sort();
    let mut expected = vec![a.clone(), b.clone()];
    expected.sort();
    assert_eq!(reaped, expected, "both orphans reaped, exactly once each");
    assert_eq!(summary.released_micros, 5_000); // 2k + 3k
    assert_eq!(summary.survived, 1);
    assert_eq!(summary.already_terminal, 0);

    // Money conserved: the two orphans' budget is back on Mint; the live hold's
    // 5_000 stays locked.
    assert_eq!(mint.balance(TREASURY), 15_000);

    // Postgres: the orphan rows are gone; the live tracking row remains.
    assert!(db.holds.get(&a).await.unwrap().is_none());
    assert!(db.holds.get(&b).await.unwrap().is_none());
    assert!(db.holds.get(&live).await.unwrap().is_some());

    // A signed Released receipt is persisted for each reaped orphan (the
    // system-of-record for the no-charge recovery terminal), and none for the
    // live hold.
    let ra = db.products.get(&a).await.unwrap().unwrap();
    assert_eq!(ra.outcome, ReceiptOutcome::Released);
    assert_eq!(ra.captured_micros, 0);
    assert_eq!(ra.request_id, "req_a");
    assert_eq!(ra.reserved_micros, 2_000);
    assert!(db.products.get(&b).await.unwrap().is_some());
    assert!(db.products.get(&live).await.unwrap().is_none());

    // A second sweep is a total no-op: nothing left expired, money unchanged.
    let second = reap_expired_claimed(
        &db.holds,
        &mint,
        &db.products,
        &clock,
        ttl,
        "worker-solo",
        50_000 + ttl,
        usize::MAX,
    )
    .await
    .unwrap();
    assert!(second.reaped_hold_ids.is_empty());
    assert_eq!(mint.balance(TREASURY), 15_000);
}

// ---------------------------------------------------------------------------
// 3. Concurrent reapers: FOR UPDATE SKIP LOCKED hands each expired hold to
//    exactly ONE worker; not-yet-expired holds are untouched.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn concurrent_reapers_recover_each_expired_hold_exactly_once() {
    let db = db_or_skip!("concurrent_reapers_recover_each_expired_hold_exactly_once");
    let mint = Arc::new(LedgerFakeMint::new().with_balance(TREASURY, 1_000_000));

    // Seed a batch of expired orphans plus one live hold.
    const N: usize = 12;
    let mut expired: Vec<String> = Vec::new();
    for i in 0..N {
        let id = open_and_track(
            &mint,
            &db.holds,
            &format!("idem_x_{i}"),
            1_000,
            0,
            None,
            &format!("req_x_{i}"),
        )
        .await;
        expired.push(id);
    }
    let live = open_and_track(
        &mint,
        &db.holds,
        "idem_live",
        7_000,
        100_000,
        None,
        "req_live",
    )
    .await;

    let holds = Arc::new(db.holds.clone());
    let products = Arc::new(db.products.clone());
    let ttl: i64 = 30_000;
    let now: i64 = 50_000;

    let spawn_worker = |name: &'static str| {
        let holds = holds.clone();
        let products = products.clone();
        let mint = mint.clone();
        tokio::spawn(async move {
            let clock = ManualClock::new(now);
            reap_expired_claimed(
                holds.as_ref(),
                mint.as_ref(),
                products.as_ref(),
                &clock,
                ttl,
                name,
                now + ttl,
                usize::MAX,
            )
            .await
            .unwrap()
        })
    };

    let (a, b) = tokio::join!(spawn_worker("worker-a"), spawn_worker("worker-b"));
    let (a, b) = (a.unwrap(), b.unwrap());

    // The union of both workers' reaped ids covers every expired hold...
    let mut union: Vec<String> = a
        .reaped_hold_ids
        .iter()
        .chain(b.reaped_hold_ids.iter())
        .cloned()
        .collect();
    union.sort();
    let mut expected = expired.clone();
    expected.sort();
    assert_eq!(
        union, expected,
        "every expired hold recovered exactly once (SKIP LOCKED honored)"
    );

    // ...and NO hold was claimed by both workers (no double-recovery).
    let set_a: HashSet<&String> = a.reaped_hold_ids.iter().collect();
    let overlap = b
        .reaped_hold_ids
        .iter()
        .filter(|h| set_a.contains(h))
        .count();
    assert_eq!(overlap, 0, "no hold may be reaped by both workers");
    assert_eq!(a.reaped_count() + b.reaped_count(), N);

    // Total budget released equals the sum of the orphans' reservations.
    assert_eq!(a.released_micros + b.released_micros, (N as i64) * 1_000);

    // The single live hold was never touched: still tracked, still funding-locked.
    assert!(holds.get(&live).await.unwrap().is_some());
    assert!(db.products.get(&live).await.unwrap().is_none());
    // Money conservation: only the live hold's 7_000 remains reserved.
    assert_eq!(mint.balance(TREASURY), 1_000_000 - 7_000);

    // Each expired orphan has exactly one persisted Released receipt.
    for id in &expired {
        let r = db
            .products
            .get(id)
            .await
            .unwrap()
            .expect("released receipt");
        assert_eq!(r.outcome, ReceiptOutcome::Released);
        assert!(db.holds.get(id).await.unwrap().is_none());
    }
}

// ---------------------------------------------------------------------------
// 4. Receipt persistence: persist + read back (by hold id and request id),
//    idempotent on the same receipt, conflict on a divergent one.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn receipt_persistence_roundtrip_and_idempotent() {
    let db = db_or_skip!("receipt_persistence_roundtrip_and_idempotent");
    let hid = uniq("hold");
    let rid = uniq("req");
    let receipt = captured_receipt(&hid, &rid, 5_000, 3_200);

    db.products.record(&receipt).await.unwrap();
    assert_eq!(
        db.products.get(&hid).await.unwrap().as_ref(),
        Some(&receipt)
    );
    assert_eq!(
        db.products.get_by_request_id(&rid).await.unwrap().as_ref(),
        Some(&receipt)
    );

    // Idempotent replay of the SAME receipt is a clean Ok — no double-write.
    db.products.record(&receipt).await.unwrap();
    assert_eq!(
        db.products.get(&hid).await.unwrap().as_ref(),
        Some(&receipt)
    );

    // A DIFFERENT receipt for the same hold_id is a genuine conflict, never a
    // silent overwrite of a settled receipt.
    let mut tampered = receipt.clone();
    tampered.captured_micros = 4_000;
    assert_eq!(
        db.products.record(&tampered).await.unwrap_err(),
        AbacusError::IdempotencyConflict
    );
    // The originally-settled receipt is intact.
    assert_eq!(db.products.get(&hid).await.unwrap().unwrap(), receipt);

    // Unknown ids read back as a clean None.
    assert!(db.products.get("hold_absent").await.unwrap().is_none());
    assert!(db
        .products
        .get_by_request_id("req_absent")
        .await
        .unwrap()
        .is_none());
}

// ---------------------------------------------------------------------------
// 5. End-to-end money conservation across the full enforce → forward → settle
//    flow, with Postgres-backed hold tracking AND receipt persistence in the loop.
// ---------------------------------------------------------------------------

fn headers(agent: &AgentId, request_id: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert(AGENT_HEADER, HeaderValue::from_str(agent.as_str()).unwrap());
    h.insert(
        "authorization",
        HeaderValue::from_static("Bearer sk-provider-test"),
    );
    h.insert(REQUEST_HEADER, HeaderValue::from_str(request_id).unwrap());
    h
}

fn body() -> serde_json::Value {
    serde_json::json!({
        "model": "gpt-4o-mini",
        "messages": [{"role": "user", "content": "hello there"}],
        "max_tokens": 2000
    })
}

fn proxy_state(
    mint: Arc<LedgerFakeMint>,
    upstream: Arc<ScriptedUpstream>,
    budgets: Arc<FakeBudgetRepo>,
    holds: PostgresHoldStore,
    products: PostgresProductStore,
) -> AppState {
    AppState {
        mint,
        upstream,
        budgets,
        receipts: Arc::new(products),
        holds: Arc::new(holds),
        ingress_api_key: "ignored-here".into(),
        admin_api_key: None,
        pricing: Arc::new(PricingTable::builtin()),
        provider: "openai".into(),
        upstream_path: "/v1/chat/completions".into(),
    }
}

#[tokio::test]
async fn end_to_end_capture_conserves_money_with_postgres_holds_and_receipts() {
    let db = db_or_skip!("end_to_end_capture_conserves_money_with_postgres_holds_and_receipts");
    let agent = AgentId::generate();
    let treasury = AccountId::parse(TREASURY).unwrap();
    let payee = Ait1Address::parse("ait1qpayee").unwrap();

    let mint = Arc::new(LedgerFakeMint::new().with_balance(TREASURY, 1_000_000));
    let budgets = Arc::new(FakeBudgetRepo::new().with_direct_hold_agent(&agent, &treasury, &payee));
    let upstream = Arc::new(ScriptedUpstream::new().push_response(
        ScriptedUpstream::success_with_usage(Usage {
            input_tokens: 1000,
            output_tokens: 1000,
        }),
    ));
    let state = proxy_state(
        mint.clone(),
        upstream,
        budgets,
        db.holds.clone(),
        db.products.clone(),
    );

    let rid = uniq("req");
    run_flow(&state, &headers(&agent, &rid), body())
        .await
        .unwrap();

    // Exact-actual capture: 150 + 600 = 750 µ$ charged; the held remainder returned.
    assert_eq!(mint.balance(TREASURY), 1_000_000 - 750);

    // The hold tracking row was removed after the terminal settlement (durable
    // create-then-remove over real SQL).
    assert!(db.holds.list().await.unwrap().is_empty());

    // A Captured receipt is persisted to real SQL as the system-of-record, keyed
    // by the caller's stable request id.
    let stored = db.products.get_by_request_id(&rid).await.unwrap().unwrap();
    assert_eq!(stored.outcome, ReceiptOutcome::Captured);
    assert_eq!(stored.captured_micros, 750);
    assert_eq!(stored.request_id, rid);
}

#[tokio::test]
async fn end_to_end_release_charges_nothing_with_postgres_holds_and_receipts() {
    let db = db_or_skip!("end_to_end_release_charges_nothing_with_postgres_holds_and_receipts");
    let agent = AgentId::generate();
    let treasury = AccountId::parse(TREASURY).unwrap();
    let payee = Ait1Address::parse("ait1qpayee").unwrap();

    let mint = Arc::new(LedgerFakeMint::new().with_balance(TREASURY, 1_000_000));
    let budgets = Arc::new(FakeBudgetRepo::new().with_direct_hold_agent(&agent, &treasury, &payee));
    // The reserve succeeds, then the provider returns a non-2xx failure ⇒ release.
    let upstream = Arc::new(ScriptedUpstream::new().push_response(ScriptedUpstream::failure(502)));
    let state = proxy_state(
        mint.clone(),
        upstream,
        budgets,
        db.holds.clone(),
        db.products.clone(),
    );

    let rid = uniq("req");
    run_flow(&state, &headers(&agent, &rid), body())
        .await
        .unwrap();

    // ADR-0002 success-only: a failed upstream charges the agent nothing — the
    // full reservation is restored (money conserved end-to-end).
    assert_eq!(mint.balance(TREASURY), 1_000_000);
    assert!(db.holds.list().await.unwrap().is_empty());

    let stored = db.products.get_by_request_id(&rid).await.unwrap().unwrap();
    assert_eq!(stored.outcome, ReceiptOutcome::Released);
    assert_eq!(stored.captured_micros, 0);
}
