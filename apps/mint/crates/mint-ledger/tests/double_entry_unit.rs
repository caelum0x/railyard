//! Unit-boundary double-entry checks: a SINGLE deterministic op writes exactly
//! the signed journal rows that sum to zero, pinning the invariant outside the
//! proptest storms (which only exercise it under random interleavings).
//!
//! - a transfer = exactly two rows: one debit on `from`, one equal credit on
//!   `to`, sharing one content-derived `op_hash`;
//! - a hold.create = exactly two rows on the SAME account (available −A, held
//!   +A) that net to zero — the intra-account double entry.

mod common;

use common::*;

use mint_core::Micros;
use mint_ledger::{HoldCreateRequest, TransferRequest};
use mint_store::{Bucket, OpKind};

#[tokio::test]
async fn single_transfer_writes_two_rows_that_net_to_zero() {
    let ledger = deterministic_ledger();
    let (ids, _supply) = seed_accounts(&ledger, &[1_000, 0]).await;

    ledger
        .transfer(TransferRequest {
            from: ids[0].clone(),
            to: ids[1].clone(),
            amount_micros: 400,
            idem_key: "t1".into(),
            ref_: None,
        })
        .await
        .unwrap();

    let journal = ledger.store().journal_snapshot();
    let rows: Vec<_> = journal
        .iter()
        .filter(|e| e.op == OpKind::Transfer)
        .collect();

    // Exactly two rows, netting to zero.
    assert_eq!(rows.len(), 2, "one transfer == exactly two journal rows");
    let sum: i64 = rows.iter().map(|e| e.delta_micros.get()).sum();
    assert_eq!(sum, 0, "the two legs of a transfer must sum to zero");

    // One debit of −400 on `from`, one equal credit of +400 on `to`.
    let debit = rows.iter().find(|e| e.delta_micros.get() < 0).unwrap();
    let credit = rows.iter().find(|e| e.delta_micros.get() > 0).unwrap();
    assert_eq!(debit.delta_micros, Micros::new(-400));
    assert_eq!(credit.delta_micros, Micros::new(400));
    assert_eq!(debit.account_id, ids[0], "debit hits `from`");
    assert_eq!(credit.account_id, ids[1], "credit hits `to`");

    // Both legs belong to the SAME op (shared content-derived hash + idem key).
    assert_eq!(debit.op_hash, credit.op_hash, "both rows share one op_hash");
    assert_eq!(
        debit.idem_key, credit.idem_key,
        "both rows share one idem_key"
    );

    // The shared invariant helper must also accept the isolated op.
    assert_double_entry(&journal);
}

#[tokio::test]
async fn single_hold_create_writes_two_intra_account_legs_that_net_to_zero() {
    let ledger = deterministic_ledger();
    let (ids, _supply) = seed_accounts(&ledger, &[1_000]).await;

    let r = ledger
        .create_hold(HoldCreateRequest {
            from: ids[0].clone(),
            amount_micros: 250,
            idem_key: "h1".into(),
            ref_: None,
            expires_at_ms: None,
        })
        .await
        .unwrap();
    assert!(!r.idempotent_replay);

    let journal = ledger.store().journal_snapshot();
    let rows: Vec<_> = journal
        .iter()
        .filter(|e| e.op == OpKind::HoldCreate)
        .collect();

    // Exactly two rows, both on the holder, netting to zero: available −A moves
    // to held +A within the same account.
    assert_eq!(rows.len(), 2, "one hold.create == exactly two journal rows");
    let sum: i64 = rows.iter().map(|e| e.delta_micros.get()).sum();
    assert_eq!(sum, 0, "hold.create legs must net to zero");
    for e in &rows {
        assert_eq!(e.account_id, ids[0], "both legs move the holder");
        assert_eq!(e.op_hash, rows[0].op_hash, "both legs share one op_hash");
    }

    let avail_leg = rows.iter().find(|e| e.bucket == Bucket::Available).unwrap();
    let held_leg = rows.iter().find(|e| e.bucket == Bucket::Held).unwrap();
    assert_eq!(
        avail_leg.delta_micros,
        Micros::new(-250),
        "available debited"
    );
    assert_eq!(held_leg.delta_micros, Micros::new(250), "held credited");

    // The hold-specific invariant helper must also accept the isolated op.
    assert_hold_legs_net_zero(&journal);
}
