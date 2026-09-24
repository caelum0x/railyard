//! Shared harness for the cross-crate property + gate tests.
#![allow(dead_code)] // each integration-test binary uses a different subset.

use std::collections::HashMap;
use std::sync::Arc;

use mint_core::{AccountId, AccountKind, HoldId, Micros, MintError};
use mint_ledger::{
    FixedSigner, HoldCaptureRequest, HoldCreateRequest, HoldReleaseRequest, Keyring, Ledger,
};
use mint_store::{FixedClock, InMemoryStore, JournalEntry, LedgerStore, OpKind};

/// Build a deterministic in-memory ledger (fixed clock).
pub fn deterministic_ledger() -> Ledger<InMemoryStore> {
    Ledger::new(InMemoryStore::with_clock(Arc::new(FixedClock::new(
        1_700_000_000_000,
    ))))
}

/// Create `n` user accounts, each seeded with the matching opening balance.
/// Returns the ids and the total opening supply (fixed for the whole run).
pub async fn seed_accounts(
    ledger: &Ledger<InMemoryStore>,
    openings: &[i64],
) -> (Vec<AccountId>, i64) {
    let mut ids = Vec::with_capacity(openings.len());
    let mut total: i64 = 0;
    for &opening in openings {
        let id = ledger
            .create_account(AccountKind::User, None)
            .await
            .unwrap();
        if opening != 0 {
            ledger
                .store()
                .seed_account(&id, Micros::new(opening))
                .await
                .unwrap();
        }
        total = total.checked_add(opening).expect("opening supply fits i64");
        ids.push(id);
    }
    (ids, total)
}

/// Sum of `available_micros` over the live projection.
pub async fn live_total(ledger: &Ledger<InMemoryStore>, ids: &[AccountId]) -> i64 {
    let mut sum: i64 = 0;
    for id in ids {
        let b = ledger.read_balance(id).await.unwrap();
        sum = sum.checked_add(b.available_micros.get()).unwrap();
    }
    sum
}

/// The AP1 conserved quantity: Σ(available + held) over the live projection.
/// Holds move value between an account's own legs (and settle to a payee on
/// capture), so this total is invariant across create/capture/release.
pub async fn live_total_held(ledger: &Ledger<InMemoryStore>, ids: &[AccountId]) -> i64 {
    let mut sum: i64 = 0;
    for id in ids {
        let b = ledger.read_balance(id).await.unwrap();
        sum = sum
            .checked_add(b.available_micros.get())
            .and_then(|s| s.checked_add(b.held_micros.get()))
            .unwrap();
    }
    sum
}

/// The minimum `held_micros` across all accounts (held non-negativity probe).
pub async fn min_held(ledger: &Ledger<InMemoryStore>, ids: &[AccountId]) -> i64 {
    let mut m = i64::MAX;
    for id in ids {
        m = m.min(ledger.read_balance(id).await.unwrap().held_micros.get());
    }
    m
}

/// The minimum `available_micros` across all accounts (non-negativity probe).
pub async fn min_available(ledger: &Ledger<InMemoryStore>, ids: &[AccountId]) -> i64 {
    let mut m = i64::MAX;
    for id in ids {
        m = m.min(
            ledger
                .read_balance(id)
                .await
                .unwrap()
                .available_micros
                .get(),
        );
    }
    m
}

/// Assert the rebuild-from-journal projection equals the live projection
/// bit-for-bit (the AP0 acceptance gate).
pub async fn assert_rebuild_matches_live(ledger: &Ledger<InMemoryStore>) {
    let store = ledger.store();
    let rebuilt = store.rebuild_balances().await.unwrap();
    let live = store.balances_snapshot();
    assert_eq!(
        rebuilt, live,
        "rebuild-from-journal must equal live projection bit-for-bit"
    );
}

/// Double-entry checks over the journal: every `Transfer` op (grouped by
/// `op_hash`) nets to zero with exactly one negative + one equal positive row,
/// and the sum of all transfer deltas across the whole journal is zero. Seed
/// rows are single-sided opening entries and are excluded.
pub fn assert_double_entry(journal: &[JournalEntry]) {
    let mut groups: HashMap<[u8; 32], Vec<i64>> = HashMap::new();
    let mut transfer_sum: i64 = 0;
    for e in journal {
        if e.op != OpKind::Transfer {
            continue;
        }
        transfer_sum = transfer_sum.checked_add(e.delta_micros.get()).unwrap();
        groups
            .entry(*e.op_hash.as_bytes())
            .or_default()
            .push(e.delta_micros.get());
    }
    assert_eq!(transfer_sum, 0, "all transfer deltas must sum to zero");
    for (hash, deltas) in groups {
        assert_eq!(
            deltas.len(),
            2,
            "transfer op {hash:?} must have exactly two rows"
        );
        assert_eq!(deltas[0] + deltas[1], 0, "transfer op must net to zero");
        let negatives = deltas.iter().filter(|d| **d < 0).count();
        let positives = deltas.iter().filter(|d| **d > 0).count();
        assert_eq!(negatives, 1, "exactly one debit row");
        assert_eq!(positives, 1, "exactly one credit row");
    }
}

/// Total of all journal deltas (seeds + transfers). Since transfers net zero,
/// this equals the seeded opening supply — a second conservation witness.
pub fn journal_delta_total(journal: &[JournalEntry]) -> i64 {
    journal.iter().fold(0i64, |acc, e| {
        acc.checked_add(e.delta_micros.get()).unwrap()
    })
}

/// Outcome bucket for a storm step (for assertions on failure paths).
#[derive(Debug, Default, Clone, Copy)]
pub struct StormStats {
    pub applied: u64,
    pub insufficient: u64,
    pub self_or_invalid: u64,
}

/// Apply one transfer, classifying the result. Non-fatal errors (insufficient
/// funds, invalid/self) are counted, not panicked — they are expected in a
/// random storm and must have written nothing.
pub async fn apply_step(
    ledger: &Ledger<InMemoryStore>,
    stats: &mut StormStats,
    from: &AccountId,
    to: &AccountId,
    amount: i64,
    idem_key: String,
) {
    use mint_ledger::TransferRequest;
    let before = ledger.store().journal_len().await;
    let res = ledger
        .transfer(TransferRequest {
            from: from.clone(),
            to: to.clone(),
            amount_micros: amount,
            idem_key,
            ref_: None,
        })
        .await;
    match res {
        Ok(r) if !r.idempotent_replay => {
            stats.applied += 1;
            assert_eq!(
                ledger.store().journal_len().await,
                before + 2,
                "a successful transfer appends exactly two rows"
            );
        }
        Ok(_) => { /* idempotent replay: no new rows */ }
        Err(MintError::InsufficientFunds) => {
            stats.insufficient += 1;
            assert_eq!(
                ledger.store().journal_len().await,
                before,
                "insufficient-funds transfer must write nothing"
            );
        }
        Err(MintError::InvalidAmount) | Err(MintError::BadAccountId) => {
            stats.self_or_invalid += 1;
            assert_eq!(ledger.store().journal_len().await, before);
        }
        Err(other) => panic!("unexpected storm error: {other:?}"),
    }
}

/// Hold analogue of [`assert_double_entry`]: every hold op (grouped by
/// `op_hash`) must have its signed rows — across BOTH buckets — net to zero.
/// create nets to zero (avail −A + held +A); capture nets to zero
/// (held −A + payee +cap + holder +(A−cap)); release nets to zero
/// (held −A + avail +A). Non-hold ops are skipped.
pub fn assert_hold_legs_net_zero(journal: &[JournalEntry]) {
    let mut groups: HashMap<[u8; 32], (OpKind, i64, usize)> = HashMap::new();
    for e in journal {
        match e.op {
            OpKind::HoldCreate | OpKind::HoldCapture | OpKind::HoldRelease => {
                let entry = groups.entry(*e.op_hash.as_bytes()).or_insert((e.op, 0, 0));
                entry.1 = entry.1.checked_add(e.delta_micros.get()).unwrap();
                entry.2 += 1;
            }
            _ => {}
        }
    }
    for (hash, (op, sum, rows)) in groups {
        assert_eq!(sum, 0, "hold op {op:?} {hash:?} legs must net to zero");
        let expected = match op {
            OpKind::HoldCreate => 2,
            OpKind::HoldRelease => 2,
            OpKind::HoldCapture => rows, // 2 (full) or 3 (partial)
            _ => unreachable!(),
        };
        assert_eq!(rows, expected, "hold op {op:?} row count");
        if op == OpKind::HoldCapture {
            assert!((2..=3).contains(&rows), "capture has 2 or 3 rows");
        }
    }
}

/// Outcome classification for a hold storm step, mirroring [`StormStats`].
#[derive(Debug, Default, Clone, Copy)]
pub struct HoldStats {
    pub created: u64,
    pub captured: u64,
    pub released: u64,
    pub idempotent: u64,
    pub insufficient: u64,
    pub exceeds: u64,
    pub not_found: u64,
    pub bad_account: u64,
}

/// Apply a `hold.create`, classifying the result and asserting the exact
/// journal row-count delta (create = +2 rows on success, +0 on any error).
/// Returns the new hold id on success.
pub async fn apply_hold_create(
    ledger: &Ledger<InMemoryStore>,
    stats: &mut HoldStats,
    from: &AccountId,
    amount: i64,
    idem_key: String,
) -> Option<HoldId> {
    let before = ledger.store().journal_len().await;
    let res = ledger
        .create_hold(HoldCreateRequest {
            from: from.clone(),
            amount_micros: amount,
            idem_key,
            ref_: None,
            expires_at_ms: None,
        })
        .await;
    match res {
        Ok(r) if !r.idempotent_replay => {
            stats.created += 1;
            assert_eq!(ledger.store().journal_len().await, before + 2);
            Some(r.hold_id)
        }
        Ok(r) => {
            stats.idempotent += 1;
            assert_eq!(ledger.store().journal_len().await, before);
            Some(r.hold_id)
        }
        Err(MintError::InsufficientFunds) => {
            stats.insufficient += 1;
            assert_eq!(ledger.store().journal_len().await, before);
            None
        }
        Err(other) => panic!("unexpected hold.create error: {other:?}"),
    }
}

/// Apply a `hold.capture`, classifying the result and asserting the exact
/// journal row-count delta (+2 or +3 on success, +0 on any error).
pub async fn apply_hold_capture(
    ledger: &Ledger<InMemoryStore>,
    stats: &mut HoldStats,
    hold_id: &HoldId,
    amount: i64,
    to: &AccountId,
    idem_key: String,
) {
    let before = ledger.store().journal_len().await;
    let res = ledger
        .capture_hold(HoldCaptureRequest {
            hold_id: hold_id.clone(),
            amount_micros: amount,
            to: to.clone(),
            idem_key,
            ref_: None,
        })
        .await;
    match res {
        Ok(r) if !r.idempotent_replay => {
            stats.captured += 1;
            let after = ledger.store().journal_len().await;
            assert!(
                after == before + 2 || after == before + 3,
                "capture appends 2 (full) or 3 (partial) rows"
            );
        }
        Ok(_) => {
            stats.idempotent += 1;
            assert_eq!(ledger.store().journal_len().await, before);
        }
        Err(MintError::CaptureExceedsHold) => {
            stats.exceeds += 1;
            assert_eq!(ledger.store().journal_len().await, before);
        }
        Err(MintError::HoldNotFound) => {
            stats.not_found += 1;
            assert_eq!(ledger.store().journal_len().await, before);
        }
        Err(MintError::BadAccountId) => {
            stats.bad_account += 1;
            assert_eq!(ledger.store().journal_len().await, before);
        }
        Err(other) => panic!("unexpected hold.capture error: {other:?}"),
    }
}

/// Apply a `hold.release`, classifying the result and asserting the exact
/// journal row-count delta (+2 on success, +0 on any error).
pub async fn apply_hold_release(
    ledger: &Ledger<InMemoryStore>,
    stats: &mut HoldStats,
    hold_id: &HoldId,
    idem_key: String,
) {
    let before = ledger.store().journal_len().await;
    let res = ledger
        .release_hold(HoldReleaseRequest {
            hold_id: hold_id.clone(),
            idem_key,
            ref_: None,
        })
        .await;
    match res {
        Ok(r) if !r.idempotent_replay => {
            stats.released += 1;
            assert_eq!(ledger.store().journal_len().await, before + 2);
        }
        Ok(_) => {
            stats.idempotent += 1;
            assert_eq!(ledger.store().journal_len().await, before);
        }
        Err(MintError::HoldNotFound) => {
            stats.not_found += 1;
            assert_eq!(ledger.store().journal_len().await, before);
        }
        Err(other) => panic!("unexpected hold.release error: {other:?}"),
    }
}

/// The public-key-only keyring for the deterministic default store signer
/// (`FixedSigner` — a constant seed/key id). Verifying with this proves offline
/// verification needs no private key and no store access.
pub fn deterministic_keyring() -> Keyring {
    Keyring::with_signer(&FixedSigner::new())
}

/// AP2 receipt gates over a completed workload:
/// - the whole log verifies offline under the default keyring;
/// - `rebuild_receipts()` equals the live log bit-exact (foldable projection);
/// - `store.merkle_root()` equals the Merkle root over the exported snapshot;
/// - the reserve proof over the receipts equals the seeded `supply`.
pub async fn assert_receipt_invariants(ledger: &Ledger<InMemoryStore>, supply: i64) {
    use mint_ledger::{merkle_root, reserve_proof_from_receipts, verify_log};

    let store = ledger.store();
    let snapshot = store.snapshot_receipts().await;
    let keyring = deterministic_keyring();

    // 1. Offline verification of the full history (no private key needed).
    verify_log(&snapshot, &keyring).expect("valid receipt log must verify");

    // 2. Rebuild == live, byte-for-byte (receipts are a journal projection).
    let rebuilt = store.rebuild_receipts().await.unwrap();
    assert_eq!(
        rebuilt.global(),
        snapshot.as_slice(),
        "rebuilt receipt log must equal the live log bit-exact"
    );

    // 3. Merkle root agreement (store vs recomputed over the snapshot).
    assert_eq!(store.merkle_root(), merkle_root(&snapshot));

    // 4. Proof of reserves: Σ last-per-account (avail + held) == seeded supply.
    let proof = reserve_proof_from_receipts(&snapshot, Micros::new(supply));
    assert!(
        proof.ok,
        "reserve proof failed: backed {:?} != supply {supply}",
        proof.backed_micros
    );
}

/// Assert rebuild-of-holds reconstructs the live holds' status/captured/amount.
pub async fn assert_rebuild_holds_matches_live(ledger: &Ledger<InMemoryStore>) {
    let store = ledger.store();
    let rebuilt = store.rebuild_holds().await.unwrap();
    let live = store.holds_snapshot();
    assert_eq!(rebuilt.len(), live.len(), "rebuild_holds cardinality");
    for (id, rec) in &live {
        let rb = rebuilt.get(id).expect("hold present in rebuild");
        assert_eq!(rb.status, rec.status, "hold {id:?} status");
        assert_eq!(
            rb.captured_micros, rec.captured_micros,
            "hold {id:?} captured"
        );
        assert_eq!(rb.amount_micros, rec.amount_micros, "hold {id:?} amount");
        assert_eq!(rb.account_id, rec.account_id, "hold {id:?} account");
    }
}
