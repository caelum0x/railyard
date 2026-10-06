//! Live Postgres tests for the production `PgStore` **hold / stream / allowance**
//! settlement SQL — the primitives vend and the agent-runtime settle against.
//!
//! These mirror the storm/conservation invariants that today only run against
//! `InMemoryStore` (see `mint-ledger/tests/common/mod.rs`), but here they run on
//! the real SQL: `FOR UPDATE` lock scope, the idempotency read-after-insert
//! seam, and the capture/release/close arithmetic.
//!
//! Gated exactly like `pg_live.rs`: `#[ignore]` + a `DATABASE_URL` probe, so a
//! DB-less CI compiles them (proving the runtime sqlx API) but skips running.
//!
//! ```sh
//! DATABASE_URL=postgres://... cargo test -p mint-store-postgres -- --ignored
//! ```

use std::sync::Arc;

use mint_core::{op_hash, AccountId, AccountKind, Micros, MintError, TransferAmount};
use mint_receipts::FixedSigner;
use mint_store::{
    AllowanceGrantCommit, AllowanceScheme, AllowanceSpendCommit, AllowanceWindow, HoldCaptureCommit,
    HoldCreateCommit, HoldReleaseCommit, LedgerStore, MintCommit, StreamCloseCommit, StreamOpenCommit,
    StreamTickCommit, StreamUnit,
};
use mint_store_postgres::PgStore;

/// Idempotency keys are global in the ledger, so each run gets its own (a
/// re-run on the same scratch database replays nothing from the last one).
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn run_key(tag: &str) -> String {
    static RUN: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let run = RUN.get_or_init(|| {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
        format!("{nanos:x}-{}", std::process::id())
    });
    format!("{tag}:{run}")
}


async fn connect() -> Option<PgStore> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let store = PgStore::connect(&url, Arc::new(FixedSigner::new()))
        .await
        .expect("connect");
    store.run_migrations().await.expect("migrations");
    Some(store)
}

fn amt(m: i64) -> TransferAmount {
    TransferAmount::new(Micros::new(m)).expect("positive amount")
}

fn oph(tag: &str, nonce: &str) -> mint_core::OpHash {
    op_hash(&serde_json::json!({ "op": tag, "nonce": nonce }))
}

/// The AP1 conserved quantity: Σ(available + held) over the given accounts.
/// Holds/streams/allowance-settlement move value between an account's own legs
/// (and to a payee on capture/tick/transfer), so this total is invariant.
async fn total_held(store: &PgStore, ids: &[AccountId]) -> i64 {
    let mut sum: i64 = 0;
    for id in ids {
        let b = store.get_balance(id).await.unwrap();
        sum = sum
            .checked_add(b.available_micros.get())
            .and_then(|s| s.checked_add(b.held_micros.get()))
            .unwrap();
    }
    sum
}

/// Fund `to` via a mint (the only conservation-preserving way to raise supply on
/// a `PgStore`; `seed_account` is intentionally not on the trait).
async fn fund(store: &PgStore, to: &AccountId, micros: i64, nonce: &str) {
    store
        .commit_mint(MintCommit {
            to: to.clone(),
            amount: amt(micros),
            deposit_ref: format!("dep-{nonce}"),
            idem_key: run_key(&format!("mint:{nonce}")),
            op_hash: oph("mint", nonce),
        })
        .await
        .expect("mint funds");
}

// ---------------------------------------------------------------------------
// HOLD: capture / release — arithmetic, idempotency, conservation.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires a live DATABASE_URL"]
async fn hold_full_capture_conserves_and_capture_is_exactly_once() {
    let Some(store) = connect().await else {
        return;
    };
    // The reserve is global: these tests assert it unchanged, so they run one
    // at a time (a parallel test's funding would move it).
    let _serial = SERIAL.lock().await;
    let holder = store.create_account(AccountKind::User, None).await.unwrap();
    let payee = store.create_account(AccountKind::Model, None).await.unwrap();
    let ids = [holder.clone(), payee.clone()];

    fund(&store, &holder, 10_000_000, "hfc").await;
    let baseline = total_held(&store, &ids).await;
    let reserve_before = store.reserve_micros().await;

    // Open a 4µ$M hold: available 10→6, held 0→4. Conservation preserved.
    let open = store
        .commit_hold_create(HoldCreateCommit {
            from: holder.clone(),
            amount: amt(4_000_000),
            idem_key: run_key("hfc:open"),
            op_hash: oph("hold.create", "hfc"),
            ref_: None,
            expires_at_ms: None,
        })
        .await
        .unwrap();
    let hb = store.get_balance(&holder).await.unwrap();
    assert_eq!(hb.available_micros, Micros::new(6_000_000));
    assert_eq!(hb.held_micros, Micros::new(4_000_000));
    assert_eq!(total_held(&store, &ids).await, baseline, "hold.create conserves");

    // Full capture (4µ$M) to payee: holder held 4→0, payee available 0→4.
    let cap_op = oph("hold.capture", "hfc");
    let cap1 = store
        .commit_hold_capture(HoldCaptureCommit {
            hold_id: open.hold_id.clone(),
            amount: amt(4_000_000),
            to: payee.clone(),
            idem_key: run_key("hfc:cap"),
            op_hash: cap_op,
            ref_: None,
        })
        .await
        .unwrap();
    assert!(!cap1.idempotent_replay);
    let hb = store.get_balance(&holder).await.unwrap();
    let pb = store.get_balance(&payee).await.unwrap();
    assert_eq!(hb.available_micros, Micros::new(6_000_000));
    assert_eq!(hb.held_micros, Micros::zero());
    assert_eq!(pb.available_micros, Micros::new(4_000_000));
    assert_eq!(total_held(&store, &ids).await, baseline, "capture conserves");

    // Idempotent double-capture (same idem_key + op_hash): replay, NOT a second
    // debit. Balances and conservation unchanged.
    let replay = store
        .commit_hold_capture(HoldCaptureCommit {
            hold_id: open.hold_id.clone(),
            amount: amt(4_000_000),
            to: payee.clone(),
            idem_key: run_key("hfc:cap"),
            op_hash: cap_op,
            ref_: None,
        })
        .await
        .unwrap();
    assert!(replay.idempotent_replay, "same key replays");
    assert_eq!(store.get_balance(&payee).await.unwrap().available_micros, Micros::new(4_000_000));
    assert_eq!(total_held(&store, &ids).await, baseline, "replay moves nothing");

    // Fresh-key capture of the now-terminal hold: HoldNotFound (no double debit).
    let again = store
        .commit_hold_capture(HoldCaptureCommit {
            hold_id: open.hold_id.clone(),
            amount: amt(4_000_000),
            to: payee.clone(),
            idem_key: run_key("hfc:cap2"),
            op_hash: oph("hold.capture", "hfc2"),
            ref_: None,
        })
        .await;
    assert!(matches!(again, Err(MintError::HoldNotFound)), "got {again:?}");
    assert_eq!(total_held(&store, &ids).await, baseline, "rejected capture moves nothing");
    assert_eq!(store.reserve_micros().await, reserve_before, "settlement never touches reserve");

    // AP0 gate: rebuild-from-journal == live projection for both accounts.
    let rebuilt = store.rebuild_balances().await.unwrap();
    for id in &ids {
        let live = store.get_balance(id).await.unwrap();
        assert_eq!(rebuilt.get(id).unwrap().available_micros, live.available_micros);
        assert_eq!(rebuilt.get(id).unwrap().held_micros, live.held_micros);
    }
}

#[tokio::test]
#[ignore = "requires a live DATABASE_URL"]
async fn hold_partial_capture_returns_remainder_and_closes() {
    let Some(store) = connect().await else {
        return;
    };
    // The reserve is global: these tests assert it unchanged, so they run one
    // at a time (a parallel test's funding would move it).
    let _serial = SERIAL.lock().await;
    let holder = store.create_account(AccountKind::User, None).await.unwrap();
    let payee = store.create_account(AccountKind::Model, None).await.unwrap();
    let ids = [holder.clone(), payee.clone()];

    fund(&store, &holder, 10_000_000, "hpc").await;
    let baseline = total_held(&store, &ids).await;

    let open = store
        .commit_hold_create(HoldCreateCommit {
            from: holder.clone(),
            amount: amt(4_000_000),
            idem_key: run_key("hpc:open"),
            op_hash: oph("hold.create", "hpc"),
            ref_: None,
            expires_at_ms: None,
        })
        .await
        .unwrap();

    // Partial capture (1µ$M of a 4µ$M hold): payee gets 1, the 3 remainder is
    // returned to the holder's available in the SAME op, held clears to 0, and
    // the hold closes Captured.
    let cap = store
        .commit_hold_capture(HoldCaptureCommit {
            hold_id: open.hold_id.clone(),
            amount: amt(1_000_000),
            to: payee.clone(),
            idem_key: run_key("hpc:cap"),
            op_hash: oph("hold.capture", "hpc"),
            ref_: None,
        })
        .await
        .unwrap();
    assert_eq!(cap.captured_micros, Micros::new(1_000_000));
    let hb = store.get_balance(&holder).await.unwrap();
    let pb = store.get_balance(&payee).await.unwrap();
    assert_eq!(hb.held_micros, Micros::zero(), "held fully cleared");
    assert_eq!(hb.available_micros, Micros::new(9_000_000), "remainder 3 returned to holder");
    assert_eq!(pb.available_micros, Micros::new(1_000_000), "payee captured 1");
    assert_eq!(total_held(&store, &ids).await, baseline, "partial capture conserves");

    // The hold is now terminal (Captured): a release finds no open hold.
    let rel = store
        .commit_hold_release(HoldReleaseCommit {
            hold_id: open.hold_id.clone(),
            idem_key: run_key("hpc:rel"),
            op_hash: oph("hold.release", "hpc"),
            ref_: None,
        })
        .await;
    assert!(matches!(rel, Err(MintError::HoldNotFound)), "got {rel:?}");
    assert_eq!(total_held(&store, &ids).await, baseline);

    // rebuild_holds recovers the captured amount + terminal status.
    let holds = store.rebuild_holds().await.unwrap();
    let rec = holds.get(&open.hold_id).expect("hold in rebuild");
    assert_eq!(rec.captured_micros, Micros::new(1_000_000));
    assert_eq!(rec.status, mint_store::HoldStatus::Captured);
}

#[tokio::test]
#[ignore = "requires a live DATABASE_URL"]
async fn hold_release_returns_full_and_is_idempotent() {
    let Some(store) = connect().await else {
        return;
    };
    // The reserve is global: these tests assert it unchanged, so they run one
    // at a time (a parallel test's funding would move it).
    let _serial = SERIAL.lock().await;
    let holder = store.create_account(AccountKind::User, None).await.unwrap();
    let ids = [holder.clone()];

    fund(&store, &holder, 7_000_000, "hr").await;
    let baseline = total_held(&store, &ids).await;

    let open = store
        .commit_hold_create(HoldCreateCommit {
            from: holder.clone(),
            amount: amt(3_000_000),
            idem_key: run_key("hr:open"),
            op_hash: oph("hold.create", "hr"),
            ref_: None,
            expires_at_ms: None,
        })
        .await
        .unwrap();
    assert_eq!(store.get_balance(&holder).await.unwrap().held_micros, Micros::new(3_000_000));

    // Release the whole reservation: held 3→0, available back to full 7.
    let rel_op = oph("hold.release", "hr");
    store
        .commit_hold_release(HoldReleaseCommit {
            hold_id: open.hold_id.clone(),
            idem_key: run_key("hr:rel"),
            op_hash: rel_op,
            ref_: None,
        })
        .await
        .unwrap();
    let hb = store.get_balance(&holder).await.unwrap();
    assert_eq!(hb.available_micros, Micros::new(7_000_000));
    assert_eq!(hb.held_micros, Micros::zero());
    assert_eq!(total_held(&store, &ids).await, baseline, "release conserves");

    // Same-key release replays (no new rows / no double credit).
    let replay = store
        .commit_hold_release(HoldReleaseCommit {
            hold_id: open.hold_id.clone(),
            idem_key: run_key("hr:rel"),
            op_hash: rel_op,
            ref_: None,
        })
        .await
        .unwrap();
    assert!(replay.idempotent_replay);
    assert_eq!(store.get_balance(&holder).await.unwrap().available_micros, Micros::new(7_000_000));

    // Fresh-key release of the terminal hold: HoldNotFound.
    let again = store
        .commit_hold_release(HoldReleaseCommit {
            hold_id: open.hold_id.clone(),
            idem_key: run_key("hr:rel2"),
            op_hash: oph("hold.release", "hr2"),
            ref_: None,
        })
        .await;
    assert!(matches!(again, Err(MintError::HoldNotFound)), "got {again:?}");
    assert_eq!(total_held(&store, &ids).await, baseline);
}

#[tokio::test]
#[ignore = "requires a live DATABASE_URL"]
async fn concurrent_double_capture_exactly_one_wins_and_conserves() {
    let Some(store) = connect().await else {
        return;
    };
    // The reserve is global: these tests assert it unchanged, so they run one
    // at a time (a parallel test's funding would move it).
    let _serial = SERIAL.lock().await;
    let store = Arc::new(store);
    let holder = store.create_account(AccountKind::User, None).await.unwrap();
    let payee = store.create_account(AccountKind::Model, None).await.unwrap();
    let ids = [holder.clone(), payee.clone()];

    fund(&store, &holder, 10_000_000, "cc").await;
    let baseline = total_held(&store, &ids).await;

    let open = store
        .commit_hold_create(HoldCreateCommit {
            from: holder.clone(),
            amount: amt(4_000_000),
            idem_key: run_key("cc:open"),
            op_hash: oph("hold.create", "cc"),
            ref_: None,
            expires_at_ms: None,
        })
        .await
        .unwrap();

    // Two genuine concurrent captures of the SAME hold under DISTINCT idem keys
    // (so neither is an idempotency replay). The hold-row FOR UPDATE lock must
    // serialize them: exactly one captures, the other sees status=Captured and
    // fails HoldNotFound — never a double debit.
    let mk = |key: &'static str| {
        let store = store.clone();
        let hold_id = open.hold_id.clone();
        let payee = payee.clone();
        tokio::spawn(async move {
            store
                .commit_hold_capture(HoldCaptureCommit {
                    hold_id,
                    amount: amt(4_000_000),
                    to: payee,
                    idem_key: run_key(key),
                    op_hash: oph("hold.capture", key),
                    ref_: None,
                })
                .await
        })
    };
    let (r1, r2) = tokio::join!(mk("cc:capA"), mk("cc:capB"));
    let results = [r1.unwrap(), r2.unwrap()];
    let wins = results.iter().filter(|r| matches!(r, Ok(o) if !o.idempotent_replay)).count();
    let losses = results
        .iter()
        .filter(|r| matches!(r, Err(MintError::HoldNotFound)))
        .count();
    assert_eq!(wins, 1, "exactly one capture wins: {results:?}");
    assert_eq!(losses, 1, "the other is HoldNotFound: {results:?}");

    let hb = store.get_balance(&holder).await.unwrap();
    let pb = store.get_balance(&payee).await.unwrap();
    assert_eq!(hb.held_micros, Micros::zero());
    assert_eq!(pb.available_micros, Micros::new(4_000_000), "payee credited exactly once");
    assert_eq!(total_held(&store, &ids).await, baseline, "conservation holds under the race");
}

// ---------------------------------------------------------------------------
// STREAM: tick accrual + close settles once, idempotent close.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires a live DATABASE_URL"]
async fn stream_tick_accrues_and_close_settles_once() {
    let Some(store) = connect().await else {
        return;
    };
    // The reserve is global: these tests assert it unchanged, so they run one
    // at a time (a parallel test's funding would move it).
    let _serial = SERIAL.lock().await;
    let payer = store.create_account(AccountKind::User, None).await.unwrap();
    let payee = store.create_account(AccountKind::Model, None).await.unwrap();
    let ids = [payer.clone(), payee.clone()];

    fund(&store, &payer, 10_000_000, "st").await;
    let baseline = total_held(&store, &ids).await;

    // Open: reserve the whole 5µ$M cap (available 10→5, held 0→5).
    let open = store
        .commit_stream_open(StreamOpenCommit {
            from: payer.clone(),
            to: payee.clone(),
            rate: amt(1_000_000),
            cap: amt(5_000_000),
            unit: StreamUnit::Unit,
            idem_key: run_key("st:open"),
            op_hash: oph("stream.open", "st"),
            ref_: None,
        })
        .await
        .unwrap();
    assert_eq!(total_held(&store, &ids).await, baseline, "open conserves");

    // Tick 2 units @1µ$M = 2 settled: payer held 5→3, payee available 0→2.
    let tick_op = oph("stream.tick", "st1");
    let tick = store
        .commit_stream_tick(StreamTickCommit {
            stream_id: open.stream_id.clone(),
            units: 2,
            seq: 1,
            idem_key: run_key("st:t1"),
            op_hash: tick_op,
            ref_: None,
        })
        .await
        .unwrap();
    assert_eq!(tick.settled_total_micros, Micros::new(2_000_000));
    assert_eq!(store.get_balance(&payee).await.unwrap().available_micros, Micros::new(2_000_000));
    assert_eq!(store.get_balance(&payer).await.unwrap().held_micros, Micros::new(3_000_000));
    assert_eq!(total_held(&store, &ids).await, baseline, "tick conserves");

    // Idempotent tick (same idem_key + op_hash): replay, no second accrual.
    let replay = store
        .commit_stream_tick(StreamTickCommit {
            stream_id: open.stream_id.clone(),
            units: 2,
            seq: 1,
            idem_key: run_key("st:t1"),
            op_hash: tick_op,
            ref_: None,
        })
        .await
        .unwrap();
    assert!(replay.idempotent_replay);
    assert_eq!(store.get_balance(&payee).await.unwrap().available_micros, Micros::new(2_000_000));

    // Close with no final tick: release the unused remainder (5-2=3) back to the
    // payer's available; settled stays 2.
    let close_op = oph("stream.close", "st");
    let close = store
        .commit_stream_close(StreamCloseCommit {
            stream_id: open.stream_id.clone(),
            units: 0,
            seq: 0,
            idem_key: run_key("st:close"),
            op_hash: close_op,
            ref_: None,
        })
        .await
        .unwrap();
    assert_eq!(close.settled_total_micros, Micros::new(2_000_000));
    assert_eq!(close.released_micros, Micros::new(3_000_000));
    let payer_b = store.get_balance(&payer).await.unwrap();
    assert_eq!(payer_b.available_micros, Micros::new(8_000_000), "5 kept + 3 released");
    assert_eq!(payer_b.held_micros, Micros::zero());
    assert_eq!(total_held(&store, &ids).await, baseline, "close conserves");

    // Idempotent close: replay, no double release.
    let creplay = store
        .commit_stream_close(StreamCloseCommit {
            stream_id: open.stream_id.clone(),
            units: 0,
            seq: 0,
            idem_key: run_key("st:close"),
            op_hash: close_op,
            ref_: None,
        })
        .await
        .unwrap();
    assert!(creplay.idempotent_replay);
    assert_eq!(store.get_balance(&payer).await.unwrap().available_micros, Micros::new(8_000_000));

    // Closed stream rejects further ticks.
    let post = store
        .commit_stream_tick(StreamTickCommit {
            stream_id: open.stream_id.clone(),
            units: 1,
            seq: 2,
            idem_key: run_key("st:t2"),
            op_hash: oph("stream.tick", "st2"),
            ref_: None,
        })
        .await;
    assert!(matches!(post, Err(MintError::StreamClosed)), "got {post:?}");

    // Projection gate: rebuild_streams recovers settled + terminal status.
    let streams = store.rebuild_streams().await.unwrap();
    let rec = streams.get(&open.stream_id).expect("stream in rebuild");
    assert_eq!(rec.settled_micros, Micros::new(2_000_000));
    assert_eq!(rec.status, mint_store::StreamStatus::Closed);
}

// ---------------------------------------------------------------------------
// ALLOWANCE: spend within / over budget + projection-rebuild gate.
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires a live DATABASE_URL"]
async fn allowance_spend_enforces_budget_and_projection_matches() {
    let Some(store) = connect().await else {
        return;
    };
    // The reserve is global: these tests assert it unchanged, so they run one
    // at a time (a parallel test's funding would move it).
    let _serial = SERIAL.lock().await;
    let granter = store.create_account(AccountKind::User, None).await.unwrap();
    let payee = store.create_account(AccountKind::Model, None).await.unwrap();
    let ids = [granter.clone(), payee.clone()];

    fund(&store, &granter, 10_000_000, "al").await;
    let baseline = total_held(&store, &ids).await;

    let grant = store
        .commit_allowance_grant(AllowanceGrantCommit {
            granter: granter.clone(),
            to_key: "delegate-key".into(),
            limit: amt(5_000_000),
            window: AllowanceWindow::PerCall,
            scope: None,
            idem_key: run_key("al:grant"),
            op_hash: oph("allowance.grant", "al"),
        })
        .await
        .unwrap();

    // Spend 3µ$M within the 5µ$M budget (transfer scheme): granter 10→7, payee
    // 0→3, remaining 2. Conservation preserved (money moves within the set).
    let s1 = store
        .commit_allowance_spend(AllowanceSpendCommit {
            allowance_id: grant.allowance_id.clone(),
            amount: amt(3_000_000),
            to: payee.clone(),
            scheme: AllowanceScheme::Transfer,
            ref_: None,
            idem_key: run_key("al:s1"),
            op_hash: oph("allowance.spend", "al1"),
        })
        .await
        .unwrap();
    assert_eq!(s1.remaining_micros, Micros::new(2_000_000));
    assert_eq!(store.get_balance(&granter).await.unwrap().available_micros, Micros::new(7_000_000));
    assert_eq!(store.get_balance(&payee).await.unwrap().available_micros, Micros::new(3_000_000));
    assert_eq!(total_held(&store, &ids).await, baseline, "spend conserves");

    // Spend 3µ$M more would total 6 > 5 limit: AllowanceExceeded, no money moved.
    let over = store
        .commit_allowance_spend(AllowanceSpendCommit {
            allowance_id: grant.allowance_id.clone(),
            amount: amt(3_000_000),
            to: payee.clone(),
            scheme: AllowanceScheme::Transfer,
            ref_: None,
            idem_key: run_key("al:s2"),
            op_hash: oph("allowance.spend", "al2"),
        })
        .await;
    assert!(matches!(over, Err(MintError::AllowanceExceeded)), "got {over:?}");
    assert_eq!(store.get_balance(&granter).await.unwrap().available_micros, Micros::new(7_000_000), "over-spend moved nothing");
    assert_eq!(total_held(&store, &ids).await, baseline);

    // Spend the exact remaining 2µ$M: succeeds, remaining 0.
    let s3 = store
        .commit_allowance_spend(AllowanceSpendCommit {
            allowance_id: grant.allowance_id.clone(),
            amount: amt(2_000_000),
            to: payee.clone(),
            scheme: AllowanceScheme::Transfer,
            ref_: None,
            idem_key: run_key("al:s3"),
            op_hash: oph("allowance.spend", "al3"),
        })
        .await
        .unwrap();
    assert_eq!(s3.remaining_micros, Micros::zero());
    assert_eq!(store.get_balance(&payee).await.unwrap().available_micros, Micros::new(5_000_000));
    assert_eq!(total_held(&store, &ids).await, baseline, "final spend conserves");

    // Projection gate: rebuild-from-events == live projection for both accounts.
    let rebuilt = store.rebuild_balances().await.unwrap();
    for id in &ids {
        let live = store.get_balance(id).await.unwrap();
        assert_eq!(rebuilt.get(id).unwrap().available_micros, live.available_micros, "avail rebuild");
        assert_eq!(rebuilt.get(id).unwrap().held_micros, live.held_micros, "held rebuild");
    }
}
