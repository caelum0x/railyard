//! AP4 §6 conservation gate — a deterministic proptest storm interleaving
//! transfers + holds + streams + mint/redeem on ONE store, asserting after every
//! step that `Σ(available + held) == reserve` (the reserve proof holds), the
//! rebuild-from-journal projection equals the live projection bit-exact, and the
//! signed receipt hash-chain still verifies offline.
//!
//! Every op flows through the store's atomic apply path, so mint/redeem move the
//! reserve in lockstep with the account leg while transfers/holds/streams are
//! Σ-neutral — the equality is a loop invariant over any interleaving.

use std::sync::Arc;

use mint_core::{
    op_hash, AccountId, AccountKind, HoldId, Micros, MintError, StreamId, TransferAmount,
};
use mint_receipts::{reserve_proof_from_receipts, verify_log, FixedSigner, Keyring};
use mint_store::{
    FixedClock, HoldCaptureCommit, HoldCreateCommit, HoldReleaseCommit, InMemoryStore, LedgerStore,
    MintCommit, RedeemCommit, StreamCloseCommit, StreamOpenCommit, StreamTickCommit, StreamUnit,
    TransferCommit,
};
use proptest::prelude::*;
use serde_json::json;

/// A live stream's settlement state (so ticks/closes respect cap + monotonic seq).
struct StreamState {
    id: StreamId,
    rate: i64,
    cap: i64,
    settled: i64,
    seq: u64,
}

/// A live hold (holder + reserved amount) so captures stay `<= held`.
struct HoldState {
    id: HoldId,
    holder: AccountId,
    amount: i64,
}

fn amt(a: i64) -> TransferAmount {
    TransferAmount::new(Micros::new(a)).unwrap()
}

async fn assert_invariants(store: &InMemoryStore, keyring: &Keyring) {
    // 1. Σ(available + held) == reserve (the §6 conservation invariant).
    let reserve = store.reserve_micros().await;
    let snapshot = store.snapshot_receipts().await;
    let proof = reserve_proof_from_receipts(&snapshot, reserve);
    assert!(
        proof.ok,
        "conservation broken: backed {:?} != reserve {:?}",
        proof.backed_micros, reserve
    );

    // 2. rebuild-from-journal == live projection, bit-exact.
    let rebuilt = store.rebuild_balances().await.unwrap();
    assert_eq!(rebuilt, store.balances_snapshot(), "rebuild != live");

    // 3. Non-negativity on both legs.
    for b in store.balances_snapshot().values() {
        assert!(!b.available_micros.is_negative(), "available went negative");
        assert!(!b.held_micros.is_negative(), "held went negative");
    }

    // 4. The signed hash-chain verifies offline.
    verify_log(&snapshot, keyring).expect("receipt chain must verify");
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn mixed_storm_preserves_conservation_and_chain(
        ops in prop::collection::vec(
            (0u8..10, 0usize..4, 0usize..4, 1i64..500),
            0..40usize,
        )
    ) {
        let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
        rt.block_on(async move {
            let store = InMemoryStore::with_clock(Arc::new(FixedClock::new(1_700_000_000_000)));
            let keyring = Keyring::with_signer(&FixedSigner::new());

            // Four accounts; funded exclusively via `mint` so reserve == Σ backing.
            let mut ids = Vec::new();
            for _ in 0..4 {
                ids.push(store.create_account(AccountKind::User, None).await.unwrap());
            }
            // Seed each with an initial confirmed-deposit mint so transfers/holds
            // have something to move.
            for (i, id) in ids.iter().enumerate() {
                store
                    .commit_mint(MintCommit {
                        to: id.clone(),
                        amount: amt(100_000),
                        deposit_ref: format!("seed-d{i}"),
                        idem_key: format!("seed-m{i}"),
                        op_hash: op_hash(&json!({"op":"mint","seed":i})),
                    })
                    .await
                    .unwrap();
            }
            assert_invariants(&store, &keyring).await;

            let mut holds: Vec<HoldState> = Vec::new();
            let mut streams: Vec<StreamState> = Vec::new();

            for (step, (sel, ai, bi, a)) in ops.into_iter().enumerate() {
                let from = ids[ai].clone();
                let to = ids[bi].clone();
                let k = format!("op{step}");
                let oh = |tag: &str| op_hash(&json!({"tag": tag, "step": step, "a": a}));

                match sel {
                    // ---- mint (reserve rises) --------------------------------
                    0 | 1 => {
                        store
                            .commit_mint(MintCommit {
                                to: to.clone(),
                                amount: amt(a),
                                deposit_ref: format!("d{step}"),
                                idem_key: format!("m{step}"),
                                op_hash: oh("mint"),
                            })
                            .await
                            .unwrap();
                    }
                    // ---- redeem (reserve falls) ------------------------------
                    2 => {
                        let res = store
                            .commit_redeem(RedeemCommit {
                                from: from.clone(),
                                amount: amt(a),
                                payout_ref: format!("p{step}"),
                                idem_key: format!("r{step}"),
                                op_hash: oh("redeem"),
                                bypass_lifecycle: false,
                            })
                            .await;
                        expect_ok_or_benign(res);
                    }
                    // ---- transfer (Σ-neutral) --------------------------------
                    3 | 4 => {
                        let res = store
                            .commit_transfer(TransferCommit {
                                from: from.clone(),
                                to: to.clone(),
                                amount: amt(a),
                                idem_key: k.clone(),
                                op_hash: oh("xfer"),
                                ref_: None,
                            })
                            .await;
                        expect_ok_or_benign(res);
                    }
                    // ---- hold create -----------------------------------------
                    5 => {
                        match store
                            .commit_hold_create(HoldCreateCommit {
                                from: from.clone(),
                                amount: amt(a),
                                idem_key: k.clone(),
                                op_hash: oh("hcreate"),
                                ref_: None,
                                expires_at_ms: None,
                            })
                            .await
                        {
                            Ok(o) => holds.push(HoldState { id: o.hold_id, holder: from, amount: a }),
                            Err(MintError::InsufficientFunds) => {}
                            Err(e) => panic!("hold create: {e:?}"),
                        }
                    }
                    // ---- hold capture / release ------------------------------
                    6 => {
                        if let Some(h) = holds.pop() {
                            // capture to a DIFFERENT account than the holder.
                            let payee = ids.iter().find(|id| **id != h.holder).unwrap().clone();
                            store
                                .commit_hold_capture(HoldCaptureCommit {
                                    hold_id: h.id,
                                    amount: amt(h.amount),
                                    to: payee,
                                    idem_key: k.clone(),
                                    op_hash: oh("hcap"),
                                    ref_: None,
                                })
                                .await
                                .unwrap();
                        }
                    }
                    7 => {
                        if let Some(h) = holds.pop() {
                            store
                                .commit_hold_release(HoldReleaseCommit {
                                    hold_id: h.id,
                                    idem_key: k.clone(),
                                    op_hash: oh("hrel"),
                                    ref_: None,
                                })
                                .await
                                .unwrap();
                        }
                    }
                    // ---- stream open -----------------------------------------
                    8 => {
                        if from != to {
                            let rate = a;
                            let cap = a * 10; // room for ~10 ticks
                            match store
                                .commit_stream_open(StreamOpenCommit {
                                    from: from.clone(),
                                    to: to.clone(),
                                    rate: amt(rate),
                                    cap: amt(cap),
                                    unit: StreamUnit::Unit,
                                    idem_key: k.clone(),
                                    op_hash: oh("sopen"),
                                    ref_: None,
                                })
                                .await
                            {
                                Ok(o) => streams.push(StreamState {
                                    id: o.stream_id,
                                    rate,
                                    cap,
                                    settled: 0,
                                    seq: 0,
                                }),
                                Err(MintError::InsufficientFunds) => {}
                                Err(e) => panic!("stream open: {e:?}"),
                            }
                        }
                    }
                    // ---- stream tick, else close -----------------------------
                    _ => {
                        if let Some(s) = streams.last_mut() {
                            let remaining = s.cap - s.settled;
                            if remaining >= s.rate {
                                // one unit fits under the cap.
                                s.seq += 1;
                                s.settled += s.rate;
                                store
                                    .commit_stream_tick(StreamTickCommit {
                                        stream_id: s.id.clone(),
                                        units: 1,
                                        seq: s.seq,
                                        idem_key: k.clone(),
                                        op_hash: oh("stick"),
                                        ref_: None,
                                    })
                                    .await
                                    .unwrap();
                            } else {
                                // no room: close it (releases the remainder).
                                let s = streams.pop().unwrap();
                                store
                                    .commit_stream_close(StreamCloseCommit {
                                        stream_id: s.id,
                                        units: 0,
                                        seq: s.seq + 1,
                                        idem_key: k.clone(),
                                        op_hash: oh("sclose"),
                                        ref_: None,
                                    })
                                    .await
                                    .unwrap();
                            }
                        }
                    }
                }

                assert_invariants(&store, &keyring).await;
            }

            // Close out any still-open streams and re-check.
            for s in streams {
                store
                    .commit_stream_close(StreamCloseCommit {
                        stream_id: s.id,
                        units: 0,
                        seq: s.seq + 1,
                        idem_key: format!("final-close-{}", s.settled),
                        op_hash: op_hash(&json!({"tag":"final","settled": s.settled})),
                        ref_: None,
                    })
                    .await
                    .unwrap();
            }
            assert_invariants(&store, &keyring).await;

            // Final receipt-projection gate: rebuild == live, bit-exact, across
            // the whole mixed-op journal (Stream*/Mint/Redeem arms included).
            let rebuilt = store.rebuild_receipts().await.unwrap();
            assert_eq!(
                rebuilt.global(),
                store.snapshot_receipts().await.as_slice(),
                "rebuilt receipt log must equal the live log byte-for-byte"
            );
        });
    }
}

/// A storm step may fail benignly (insufficient funds / self-transfer); such a
/// step must have written nothing, which `assert_invariants` re-checks.
fn expect_ok_or_benign<T>(res: Result<T, MintError>) {
    match res {
        Ok(_) => {}
        Err(MintError::InsufficientFunds)
        | Err(MintError::ReserveInsufficient)
        | Err(MintError::BadAccountId) => {}
        Err(e) => panic!("unexpected storm error: {e:?}"),
    }
}
