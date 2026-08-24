//! AP4 stream integration tests: open reserves cap, tick settles `units*rate`
//! monotonically, out-of-order/duplicate seq rejected, close settles final +
//! releases unused, and the whole family is conservation-NEUTRAL (never touches
//! the reserve) with a bit-exact rebuild.

use std::sync::Arc;

use mint_core::{AccountId, AccountKind, Micros, MintError};
use mint_store::{FixedClock, InMemoryStore, LedgerStore, StreamStatus, StreamUnit};
use mint_stream::{CloseStreamRequest, OpenStreamRequest, StreamLedger, TickStreamRequest};

fn ledger() -> StreamLedger<InMemoryStore> {
    StreamLedger::new(InMemoryStore::with_clock(Arc::new(FixedClock::new(
        1_700_000_000_000,
    ))))
}

async fn account(l: &StreamLedger<InMemoryStore>, kind: AccountKind, seed: i64) -> AccountId {
    let id = l.store().create_account(kind, None).await.unwrap();
    if seed != 0 {
        l.store()
            .seed_account(&id, Micros::new(seed))
            .await
            .unwrap();
    }
    id
}

fn open(from: &AccountId, to: &AccountId, rate: i64, cap: i64, key: &str) -> OpenStreamRequest {
    OpenStreamRequest {
        from: from.clone(),
        to: to.clone(),
        rate_micros: rate,
        cap_micros: cap,
        unit: StreamUnit::Unit,
        idem_key: key.into(),
        ref_: None,
    }
}

fn tick(stream_id: &str, units: u64, seq: u64, key: &str) -> TickStreamRequest {
    TickStreamRequest {
        stream_id: stream_id.into(),
        units,
        seq,
        idem_key: key.into(),
        ref_: None,
    }
}

/// Σ(available + held) over the given accounts.
async fn total_backing(l: &StreamLedger<InMemoryStore>, ids: &[AccountId]) -> i64 {
    let mut sum = 0i64;
    for id in ids {
        let b = l.store().get_balance(id).await.unwrap();
        sum += b.available_micros.get() + b.held_micros.get();
    }
    sum
}

#[tokio::test]
async fn open_reserves_the_whole_cap() {
    let l = ledger();
    let a = account(&l, AccountKind::User, 10_000_000).await;
    let b = account(&l, AccountKind::Model, 0).await;
    let jbefore = l.store().journal_len().await;
    let rbefore = l.store().receipts_snapshot().len();

    let r = l
        .open_stream(open(&a, &b, 1_000, 4_000_000, "s1"))
        .await
        .unwrap();
    assert!(!r.idempotent_replay);
    // available dropped by cap, held rose by cap.
    let ab = l.store().get_balance(&a).await.unwrap();
    assert_eq!(ab.available_micros, Micros::new(6_000_000));
    assert_eq!(ab.held_micros, Micros::new(4_000_000));
    // exactly 2 journal rows + 1 receipt.
    assert_eq!(l.store().journal_len().await, jbefore + 2);
    assert_eq!(l.store().receipts_snapshot().len(), rbefore + 1);
    // record opened.
    let rec = l.read_stream(&r.stream_id).await.unwrap();
    assert_eq!(rec.status, StreamStatus::Open);
    assert_eq!(rec.settled_micros, Micros::zero());
    assert_eq!(rec.last_seq, 0);
    assert_eq!(rec.cap_micros, Micros::new(4_000_000));
    // reserve is untouched (streams are conservation-neutral).
    assert_eq!(l.store().reserve_micros_snapshot(), Micros::zero());

    // Idempotent replay → SAME stream id, no new rows.
    let replay = l
        .open_stream(open(&a, &b, 1_000, 4_000_000, "s1"))
        .await
        .unwrap();
    assert!(replay.idempotent_replay);
    assert_eq!(replay.stream_id, r.stream_id);
    assert_eq!(l.store().journal_len().await, jbefore + 2);

    // Same key, different body → conflict.
    assert_eq!(
        l.open_stream(open(&a, &b, 1_000, 5_000_000, "s1"))
            .await
            .unwrap_err(),
        MintError::IdempotencyConflict
    );
}

#[tokio::test]
async fn open_rejects_bad_amounts_and_self_stream() {
    let l = ledger();
    let a = account(&l, AccountKind::User, 10_000_000).await;
    let b = account(&l, AccountKind::Model, 0).await;
    assert_eq!(
        l.open_stream(open(&a, &b, 0, 4_000_000, "s1"))
            .await
            .unwrap_err(),
        MintError::InvalidAmount
    );
    assert_eq!(
        l.open_stream(open(&a, &b, 1_000, 0, "s2"))
            .await
            .unwrap_err(),
        MintError::InvalidAmount
    );
    assert_eq!(
        l.open_stream(open(&a, &a, 1_000, 4_000_000, "s3"))
            .await
            .unwrap_err(),
        MintError::BadAccountId
    );
}

#[tokio::test]
async fn tick_settles_units_times_rate() {
    let l = ledger();
    let a = account(&l, AccountKind::User, 10_000_000).await;
    let b = account(&l, AccountKind::Model, 0).await;
    let s = l
        .open_stream(open(&a, &b, 1_000, 4_000_000, "s1"))
        .await
        .unwrap();
    let sid = s.stream_id.as_str().to_string();

    let t1 = l.tick_stream(tick(&sid, 3, 1, "t1")).await.unwrap();
    assert_eq!(t1.settled_total_micros, Micros::new(3_000));
    assert_eq!(
        l.store().get_balance(&b).await.unwrap().available_micros,
        Micros::new(3_000)
    );
    let ab = l.store().get_balance(&a).await.unwrap();
    assert_eq!(ab.held_micros, Micros::new(4_000_000 - 3_000));
    assert_eq!(
        l.read_stream(&s.stream_id).await.unwrap().status,
        StreamStatus::Open
    );

    let t2 = l.tick_stream(tick(&sid, 2, 2, "t2")).await.unwrap();
    assert_eq!(t2.settled_total_micros, Micros::new(5_000));
    assert_eq!(
        l.store().get_balance(&b).await.unwrap().available_micros,
        Micros::new(5_000)
    );

    // A zero-unit tick is rejected as an invalid amount (writes nothing).
    let j = l.store().journal_len().await;
    assert_eq!(
        l.tick_stream(tick(&sid, 0, 3, "t0")).await.unwrap_err(),
        MintError::InvalidAmount
    );
    assert_eq!(l.store().journal_len().await, j);
}

#[tokio::test]
async fn tick_seq_must_strictly_increase() {
    let l = ledger();
    let a = account(&l, AccountKind::User, 10_000_000).await;
    let b = account(&l, AccountKind::Model, 0).await;
    let s = l
        .open_stream(open(&a, &b, 1_000, 4_000_000, "s1"))
        .await
        .unwrap();
    let sid = s.stream_id.as_str().to_string();

    l.tick_stream(tick(&sid, 1, 5, "t5")).await.unwrap();
    let j = l.store().journal_len().await;
    let settled = l.read_stream(&s.stream_id).await.unwrap().settled_micros;

    // duplicate seq (distinct idem_key) → regressed, nothing moved.
    assert_eq!(
        l.tick_stream(tick(&sid, 1, 5, "tdup")).await.unwrap_err(),
        MintError::StreamSeqRegressed
    );
    // lower seq → regressed.
    assert_eq!(
        l.tick_stream(tick(&sid, 1, 3, "tlow")).await.unwrap_err(),
        MintError::StreamSeqRegressed
    );
    assert_eq!(l.store().journal_len().await, j);
    assert_eq!(
        l.read_stream(&s.stream_id).await.unwrap().settled_micros,
        settled
    );

    // next higher seq succeeds.
    l.tick_stream(tick(&sid, 1, 6, "t6")).await.unwrap();
    assert_eq!(l.read_stream(&s.stream_id).await.unwrap().last_seq, 6);
}

#[tokio::test]
async fn tick_cannot_exceed_cap() {
    let l = ledger();
    let a = account(&l, AccountKind::User, 10_000_000).await;
    let b = account(&l, AccountKind::Model, 0).await;
    // cap 5_000, rate 1_000 → 5 units max.
    let s = l
        .open_stream(open(&a, &b, 1_000, 5_000, "s1"))
        .await
        .unwrap();
    let sid = s.stream_id.as_str().to_string();

    l.tick_stream(tick(&sid, 3, 1, "t1")).await.unwrap(); // settled 3_000
                                                          // 3 more units (total 6_000) would exceed cap 5_000.
    let j = l.store().journal_len().await;
    assert_eq!(
        l.tick_stream(tick(&sid, 3, 2, "t2")).await.unwrap_err(),
        MintError::StreamCapExceeded
    );
    assert_eq!(l.store().journal_len().await, j); // no partial application
                                                  // exactly landing on cap (2 more units) succeeds.
    let ok = l.tick_stream(tick(&sid, 2, 2, "t2b")).await.unwrap();
    assert_eq!(ok.settled_total_micros, Micros::new(5_000));
}

#[tokio::test]
async fn close_settles_final_and_releases_remainder() {
    let l = ledger();
    let a = account(&l, AccountKind::User, 10_000_000).await;
    let b = account(&l, AccountKind::Model, 0).await;
    // cap 100_000, rate 1_000 → 100 units.
    let s = l
        .open_stream(open(&a, &b, 1_000, 100_000, "s1"))
        .await
        .unwrap();
    let sid = s.stream_id.as_str().to_string();
    l.tick_stream(tick(&sid, 30, 1, "t1")).await.unwrap(); // settled 30_000

    let c = l
        .close_stream(CloseStreamRequest {
            stream_id: sid.clone(),
            units: 10,
            seq: 2,
            idem_key: "c1".into(),
            ref_: None,
        })
        .await
        .unwrap();
    assert_eq!(c.settled_total_micros, Micros::new(40_000));
    assert_eq!(c.released_micros, Micros::new(60_000));
    // payee got 40_000 total.
    assert_eq!(
        l.store().get_balance(&b).await.unwrap().available_micros,
        Micros::new(40_000)
    );
    // payer held cleared, remainder back in available: 9_900_000 + 60_000.
    let ab = l.store().get_balance(&a).await.unwrap();
    assert_eq!(ab.held_micros, Micros::zero());
    assert_eq!(ab.available_micros, Micros::new(10_000_000 - 40_000));
    assert_eq!(
        l.read_stream(&s.stream_id).await.unwrap().status,
        StreamStatus::Closed
    );

    // tick / close after Closed → StreamClosed.
    assert_eq!(
        l.tick_stream(tick(&sid, 1, 3, "t3")).await.unwrap_err(),
        MintError::StreamClosed
    );
    assert_eq!(
        l.close_stream(CloseStreamRequest {
            stream_id: sid,
            units: 0,
            seq: 9,
            idem_key: "c2".into(),
            ref_: None,
        })
        .await
        .unwrap_err(),
        MintError::StreamClosed
    );
}

#[tokio::test]
async fn close_with_zero_units_releases_full_remainder() {
    let l = ledger();
    let a = account(&l, AccountKind::User, 10_000_000).await;
    let b = account(&l, AccountKind::Model, 0).await;
    let s = l
        .open_stream(open(&a, &b, 1_000, 50_000, "s1"))
        .await
        .unwrap();
    let sid = s.stream_id.as_str().to_string();
    l.tick_stream(tick(&sid, 10, 1, "t1")).await.unwrap(); // settled 10_000

    let c = l
        .close_stream(CloseStreamRequest {
            stream_id: sid,
            units: 0,
            seq: 2,
            idem_key: "c1".into(),
            ref_: None,
        })
        .await
        .unwrap();
    assert_eq!(c.settled_total_micros, Micros::new(10_000));
    assert_eq!(c.released_micros, Micros::new(40_000));
    let ab = l.store().get_balance(&a).await.unwrap();
    assert_eq!(ab.held_micros, Micros::zero());
    assert_eq!(ab.available_micros, Micros::new(10_000_000 - 10_000));
}

#[tokio::test]
async fn streams_are_conservation_neutral_and_rebuild_bit_exact() {
    let l = ledger();
    let a = account(&l, AccountKind::User, 10_000_000).await;
    let b = account(&l, AccountKind::Model, 0).await;
    let c = account(&l, AccountKind::User, 3_000_000).await;
    let ids = [a.clone(), b.clone(), c.clone()];
    let backing_before = total_backing(&l, &ids).await;

    // A storm of open/tick/close across two streams.
    let s1 = l
        .open_stream(open(&a, &b, 1_000, 100_000, "s1"))
        .await
        .unwrap();
    let sid1 = s1.stream_id.as_str().to_string();
    l.tick_stream(tick(&sid1, 20, 1, "t1")).await.unwrap();
    l.tick_stream(tick(&sid1, 5, 2, "t2")).await.unwrap();

    let s2 = l
        .open_stream(open(&c, &b, 2_000, 40_000, "s2"))
        .await
        .unwrap();
    let sid2 = s2.stream_id.as_str().to_string();
    l.tick_stream(tick(&sid2, 10, 1, "t3")).await.unwrap(); // settles exactly to cap
    l.close_stream(CloseStreamRequest {
        stream_id: sid2,
        units: 0,
        seq: 2,
        idem_key: "c2".into(),
        ref_: None,
    })
    .await
    .unwrap();

    l.close_stream(CloseStreamRequest {
        stream_id: sid1,
        units: 5,
        seq: 3,
        idem_key: "c1".into(),
        ref_: None,
    })
    .await
    .unwrap();

    // Reserve never moved; Σ(avail+held) is invariant.
    assert_eq!(l.store().reserve_micros_snapshot(), Micros::zero());
    assert_eq!(total_backing(&l, &ids).await, backing_before);

    // rebuild_balances == live, bit-exact.
    let rebuilt = l.store().rebuild_balances().await.unwrap();
    assert_eq!(rebuilt, l.store().balances_snapshot());

    // rebuild_streams reconstructs status + settled + cap.
    let rstreams = l.store().rebuild_streams().await.unwrap();
    let live_streams = l.store().streams_snapshot();
    assert_eq!(rstreams.len(), live_streams.len());
    for (id, rec) in &live_streams {
        let rb = rstreams.get(id).expect("stream present in rebuild");
        assert_eq!(rb.status, rec.status, "stream {id:?} status");
        assert_eq!(
            rb.settled_micros, rec.settled_micros,
            "stream {id:?} settled"
        );
        assert_eq!(rb.cap_micros, rec.cap_micros, "stream {id:?} cap");
        assert_eq!(rb.from_id, rec.from_id);
        assert_eq!(rb.to_id, rec.to_id);
    }

    // rebuild_receipts == live receipts, bit-exact.
    let rebuilt_receipts = l.store().rebuild_receipts().await.unwrap();
    assert_eq!(
        rebuilt_receipts.global(),
        l.store().receipts_snapshot().as_slice(),
        "rebuilt receipt log must equal the live log byte-for-byte"
    );
}
