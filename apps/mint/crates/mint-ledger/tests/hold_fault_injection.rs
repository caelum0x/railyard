//! AP1 gate: the fault-injection matrix. Simulated crash between capture and
//! receipt, double-capture (same key = replay, different key = not-found),
//! release-after-capture / capture-after-release, and capture-exceeds-hold —
//! none may violate the invariants and failure never captures. After every
//! branch: Σ(available + held) == opening_total and rebuild == live incl held.

mod common;

use common::*;
use mint_core::{AccountKind, Micros, MintError};
use mint_ledger::{HoldCaptureRequest, HoldCreateRequest, HoldReleaseRequest, Ledger, LedgerStore};
use mint_store::{HoldStatus, InMemoryStore};

async fn seeded(l: &Ledger<InMemoryStore>, kind: AccountKind, seed: i64) -> mint_core::AccountId {
    let id = l.create_account(kind, None).await.unwrap();
    if seed != 0 {
        l.store()
            .seed_account(&id, Micros::new(seed))
            .await
            .unwrap();
    }
    id
}

fn create(from: &mint_core::AccountId, amount: i64, key: &str) -> HoldCreateRequest {
    HoldCreateRequest {
        from: from.clone(),
        amount_micros: amount,
        idem_key: key.to_string(),
        ref_: None,
    }
}

fn capture(
    hold_id: &mint_core::HoldId,
    amount: i64,
    to: &mint_core::AccountId,
    key: &str,
) -> HoldCaptureRequest {
    HoldCaptureRequest {
        hold_id: hold_id.clone(),
        amount_micros: amount,
        to: to.clone(),
        idem_key: key.to_string(),
        ref_: None,
    }
}

fn release(hold_id: &mint_core::HoldId, key: &str) -> HoldReleaseRequest {
    HoldReleaseRequest {
        hold_id: hold_id.clone(),
        idem_key: key.to_string(),
        ref_: None,
    }
}

/// (a) Crash between capture and receipt: discard the returned receipt and retry
/// the SAME idem_key. The retry is retry-safe — identical outcome, a single
/// application, payee credited once, exactly one set of rows.
#[tokio::test]
async fn crash_between_capture_and_receipt_is_retry_safe() {
    let l = deterministic_ledger();
    let a = seeded(&l, AccountKind::User, 10_000_000).await;
    let b = seeded(&l, AccountKind::Model, 0).await;
    let ids = [a.clone(), b.clone()];
    let opening = live_total_held(&l, &ids).await;

    let h = l.create_hold(create(&a, 4_000_000, "h")).await.unwrap();

    // First capture succeeds but we "crash" — pretend the receipt never arrived.
    let first = l
        .capture_hold(capture(&h.hold_id, 3_000_000, &b, "cap"))
        .await
        .unwrap();
    assert!(!first.idempotent_replay);
    let journal_after_first = l.store().journal_len().await;
    let bal_b = l.read_balance(&b).await.unwrap();

    // Retry with the SAME idem_key: identical outcome, no new rows, credited once.
    let retry = l
        .capture_hold(capture(&h.hold_id, 3_000_000, &b, "cap"))
        .await
        .unwrap();
    assert!(retry.idempotent_replay);
    assert_eq!(retry.first_seq, first.first_seq);
    assert_eq!(retry.holder_balance, first.holder_balance);
    assert_eq!(retry.payee_balance, first.payee_balance);
    assert_eq!(l.store().journal_len().await, journal_after_first);
    assert_eq!(l.read_balance(&b).await.unwrap(), bal_b); // payee credited once

    assert_eq!(live_total_held(&l, &ids).await, opening);
    assert_rebuild_matches_live(&l).await;
}

/// (b) Double-capture with the SAME key is an idempotent replay (already covered
/// above); (c) double-capture with a DIFFERENT key is HoldNotFound + nothing
/// written (the hold is already terminal).
#[tokio::test]
async fn double_capture_different_key_is_not_found_and_writes_nothing() {
    let l = deterministic_ledger();
    let a = seeded(&l, AccountKind::User, 10_000_000).await;
    let b = seeded(&l, AccountKind::Model, 0).await;
    let ids = [a.clone(), b.clone()];
    let opening = live_total_held(&l, &ids).await;

    let h = l.create_hold(create(&a, 4_000_000, "h")).await.unwrap();
    l.capture_hold(capture(&h.hold_id, 2_000_000, &b, "cap1"))
        .await
        .unwrap();

    let len = l.store().journal_len().await;
    let bal_b = l.read_balance(&b).await.unwrap();
    let err = l
        .capture_hold(capture(&h.hold_id, 1_000_000, &b, "cap2"))
        .await
        .unwrap_err();
    assert_eq!(err, MintError::HoldNotFound);
    assert_eq!(l.store().journal_len().await, len); // nothing written
    assert_eq!(l.read_balance(&b).await.unwrap(), bal_b); // no extra settlement

    assert_eq!(live_total_held(&l, &ids).await, opening);
    assert_rebuild_matches_live(&l).await;
}

/// (d) release-after-capture and capture-after-release are HoldNotFound with no
/// settlement change.
#[tokio::test]
async fn release_after_capture_and_capture_after_release_are_not_found() {
    let l = deterministic_ledger();
    let a = seeded(&l, AccountKind::User, 20_000_000).await;
    let b = seeded(&l, AccountKind::Model, 0).await;
    let ids = [a.clone(), b.clone()];
    let opening = live_total_held(&l, &ids).await;

    // Captured hold, then attempt release.
    let hc = l.create_hold(create(&a, 4_000_000, "hc")).await.unwrap();
    l.capture_hold(capture(&hc.hold_id, 4_000_000, &b, "cap"))
        .await
        .unwrap();
    let len1 = l.store().journal_len().await;
    let bal_before = (
        l.read_balance(&a).await.unwrap(),
        l.read_balance(&b).await.unwrap(),
    );
    assert_eq!(
        l.release_hold(release(&hc.hold_id, "rel"))
            .await
            .unwrap_err(),
        MintError::HoldNotFound
    );
    assert_eq!(l.store().journal_len().await, len1);
    assert_eq!(
        (
            l.read_balance(&a).await.unwrap(),
            l.read_balance(&b).await.unwrap()
        ),
        bal_before
    );

    // Released hold, then attempt capture.
    let hr = l.create_hold(create(&a, 3_000_000, "hr")).await.unwrap();
    l.release_hold(release(&hr.hold_id, "rel2")).await.unwrap();
    let len2 = l.store().journal_len().await;
    let bal_b = l.read_balance(&b).await.unwrap();
    assert_eq!(
        l.capture_hold(capture(&hr.hold_id, 1_000_000, &b, "cap2"))
            .await
            .unwrap_err(),
        MintError::HoldNotFound
    );
    assert_eq!(l.store().journal_len().await, len2);
    assert_eq!(l.read_balance(&b).await.unwrap(), bal_b); // no settlement

    assert_eq!(live_total_held(&l, &ids).await, opening);
    assert_rebuild_matches_live(&l).await;
    assert_rebuild_holds_matches_live(&l).await;
}

/// (e) capture-exceeds-hold is CaptureExceedsHold, nothing written, hold Open.
#[tokio::test]
async fn capture_exceeds_hold_writes_nothing_and_leaves_open() {
    let l = deterministic_ledger();
    let a = seeded(&l, AccountKind::User, 10_000_000).await;
    let b = seeded(&l, AccountKind::Model, 0).await;
    let ids = [a.clone(), b.clone()];
    let opening = live_total_held(&l, &ids).await;

    let h = l.create_hold(create(&a, 4_000_000, "h")).await.unwrap();
    let len = l.store().journal_len().await;
    assert_eq!(
        l.capture_hold(capture(&h.hold_id, 5_000_000, &b, "cap"))
            .await
            .unwrap_err(),
        MintError::CaptureExceedsHold
    );
    assert_eq!(l.store().journal_len().await, len);
    assert_eq!(
        l.store().holds_snapshot()[&h.hold_id].status,
        HoldStatus::Open
    );

    // Hold still capturable validly afterwards.
    let ok = l
        .capture_hold(capture(&h.hold_id, 4_000_000, &b, "cap2"))
        .await
        .unwrap();
    assert_eq!(ok.status, HoldStatus::Captured);

    assert_eq!(live_total_held(&l, &ids).await, opening);
    assert_rebuild_matches_live(&l).await;
}
