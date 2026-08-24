//! AP2 property + integration tests: random transfer+hold storms preserve
//! receipt-chain integrity, the log verifies offline, the Merkle root agrees,
//! the reserve proof balances, and the live log equals its journal-folded
//! rebuild bit-exact. A tampered history is always detected.

mod common;

use common::*;
use mint_core::HoldId;
use mint_ledger::{merkle_root, verify_log, ReceiptSig, VerifyError};
use proptest::prelude::*;

/// One storm step: (kind 0..4, index i, index j, amount).
type Step = (u8, usize, usize, i64);

fn step_strategy(n: usize) -> impl Strategy<Value = Step> {
    (0u8..4, 0..n, 0..n, 1i64..2_000_000)
}

fn storm_strategy() -> impl Strategy<Value = (Vec<i64>, Vec<Step>)> {
    // All openings > 0 so every account is seeded (has a genesis receipt);
    // keeps the reserve proof clean while still exercising every op.
    prop::collection::vec(1i64..1_000_000, 2..=6).prop_flat_map(|openings| {
        let n = openings.len();
        let steps = prop::collection::vec(step_strategy(n), 0..160);
        (Just(openings), steps)
    })
}

struct OpenHold {
    id: HoldId,
    amount: i64,
    holder: usize,
}

fn run_storm(openings: Vec<i64>, steps: Vec<Step>) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async move {
        let ledger = deterministic_ledger();
        let (ids, opening_total) = seed_accounts(&ledger, &openings).await;
        let n = ids.len();

        let mut tstats = StormStats::default();
        let mut hstats = HoldStats::default();
        let mut open: Vec<OpenHold> = Vec::new();

        for (idx, (kind, i, j, amount)) in steps.iter().enumerate() {
            let key = format!("op-{idx}");
            match kind {
                0 => apply_step(&ledger, &mut tstats, &ids[*i], &ids[*j], *amount, key).await,
                1 => {
                    if let Some(id) =
                        apply_hold_create(&ledger, &mut hstats, &ids[*i], *amount, key).await
                    {
                        if !open.iter().any(|h| h.id == id) {
                            open.push(OpenHold {
                                id,
                                amount: *amount,
                                holder: *i,
                            });
                        }
                    }
                }
                2 => {
                    if !open.is_empty() {
                        let pos = *i % open.len();
                        let h = &open[pos];
                        let cap = 1 + amount.rem_euclid(h.amount);
                        let mut to = *j % n;
                        if to == h.holder {
                            to = (to + 1) % n;
                        }
                        let id = h.id.clone();
                        apply_hold_capture(&ledger, &mut hstats, &id, cap, &ids[to], key).await;
                        open.remove(pos);
                    }
                }
                3 => {
                    if !open.is_empty() {
                        let pos = *i % open.len();
                        let id = open[pos].id.clone();
                        apply_hold_release(&ledger, &mut hstats, &id, key).await;
                        open.remove(pos);
                    }
                }
                _ => unreachable!(),
            }

            // The receipt chain stays valid at EVERY step (incremental integrity).
            let snap = ledger.receipts().await;
            prop_assert!(
                verify_log(&snap, &deterministic_keyring()).is_ok(),
                "receipt chain must verify at every step"
            );
        }

        // Full AP2 gate: offline verify + rebuild-bit-exact + merkle + reserve.
        assert_receipt_invariants(&ledger, opening_total).await;

        // Every op that wrote journal rows also wrote receipts: the receipt count
        // equals seeds + 1-per-transfer-leg×2 + hold receipts. Cross-check that
        // there is at least one receipt per seeded account.
        prop_assert!(ledger.receipts().await.len() >= ids.len());

        Ok(())
    })
    .unwrap();
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn storm_preserves_receipt_chain_integrity((openings, steps) in storm_strategy()) {
        run_storm(openings, steps);
    }
}

/// A full workload is exported and verified OFFLINE (no store, no private key),
/// then a single tampered receipt is rejected — the AP2 §6 tamper-evidence gate.
#[test]
fn offline_verify_accepts_valid_and_rejects_tampered() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        let ledger = deterministic_ledger();
        let (ids, supply) = seed_accounts(&ledger, &[10_000_000, 5_000_000, 0]).await;

        // A representative workload: transfer + partial capture + release.
        use mint_ledger::{
            HoldCaptureRequest, HoldCreateRequest, HoldReleaseRequest, TransferRequest,
        };
        ledger
            .transfer(TransferRequest {
                from: ids[0].clone(),
                to: ids[1].clone(),
                amount_micros: 2_000_000,
                idem_key: "t1".into(),
                ref_: None,
            })
            .await
            .unwrap();
        let h = ledger
            .create_hold(HoldCreateRequest {
                from: ids[0].clone(),
                amount_micros: 3_000_000,
                idem_key: "h1".into(),
                ref_: None,
            })
            .await
            .unwrap();
        ledger
            .capture_hold(HoldCaptureRequest {
                hold_id: h.hold_id.clone(),
                amount_micros: 1_000_000,
                to: ids[2].clone(),
                idem_key: "c1".into(),
                ref_: None,
            })
            .await
            .unwrap();
        let h2 = ledger
            .create_hold(HoldCreateRequest {
                from: ids[1].clone(),
                amount_micros: 500_000,
                idem_key: "h2".into(),
                ref_: None,
            })
            .await
            .unwrap();
        ledger
            .release_hold(HoldReleaseRequest {
                hold_id: h2.hold_id.clone(),
                idem_key: "r2".into(),
                ref_: None,
            })
            .await
            .unwrap();

        // --- OFFLINE: only the exported receipts + a public-key keyring. ---
        let exported = ledger.receipts().await;
        let keyring = deterministic_keyring();

        // 1. The honest history verifies with no store and no private key.
        assert!(verify_log(&exported, &keyring).is_ok());
        // The ledger's own thin pass-through agrees.
        assert!(ledger.verify_receipts(&keyring).await.is_ok());

        // Reserve proof + merkle agree offline too.
        assert_receipt_invariants(&ledger, supply).await;

        // 2. Tamper: mutate a historical receipt's amount → HashMismatch.
        let mut tampered = exported.clone();
        let mid = tampered.len() / 2;
        tampered[mid].body.amount_micros = mint_core::Micros::new(424_242);
        assert!(matches!(
            verify_log(&tampered, &keyring),
            Err(VerifyError::HashMismatch { .. })
        ));

        // 3. Tamper: flip one signature byte → BadSignature.
        let mut forged = exported.clone();
        let mut sig = *forged[0].sig.as_bytes();
        sig[10] ^= 0x80;
        forged[0].sig = ReceiptSig::from_bytes(sig);
        assert!(matches!(
            verify_log(&forged, &keyring),
            Err(VerifyError::BadSignature { .. })
        ));

        // 4. Tamper: drop a receipt from the middle → the surviving chain no
        //    longer links / is non-contiguous ⇒ rejected.
        let mut dropped = exported.clone();
        dropped.remove(mid);
        assert!(verify_log(&dropped, &keyring).is_err());

        // 5. Audit-level detection: mutating a receipt's leaf hash changes the
        //    Merkle root (and would fail signature/chain checks too).
        let honest_root = merkle_root(&exported);
        let mut releafed = exported.clone();
        releafed[mid].hash = mint_ledger::ReceiptHash::from_bytes([0xCD; 32]);
        assert_ne!(merkle_root(&releafed), honest_root);
    });
}

/// Idempotent replays of transfers AND every hold op append NO extra receipts,
/// so the chain advances exactly once per applied op.
#[test]
fn idempotent_replays_do_not_double_append_receipts() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        let ledger = deterministic_ledger();
        let (ids, _supply) = seed_accounts(&ledger, &[10_000_000, 0]).await;

        use mint_ledger::{HoldCreateRequest, TransferRequest};

        ledger
            .transfer(TransferRequest {
                from: ids[0].clone(),
                to: ids[1].clone(),
                amount_micros: 1_000_000,
                idem_key: "t1".into(),
                ref_: None,
            })
            .await
            .unwrap();
        let h = ledger
            .create_hold(HoldCreateRequest {
                from: ids[0].clone(),
                amount_micros: 2_000_000,
                idem_key: "h1".into(),
                ref_: None,
            })
            .await
            .unwrap();
        let count = ledger.receipts().await.len();

        // Replay both ops several times; receipt count is frozen.
        for _ in 0..4 {
            ledger
                .transfer(TransferRequest {
                    from: ids[0].clone(),
                    to: ids[1].clone(),
                    amount_micros: 1_000_000,
                    idem_key: "t1".into(),
                    ref_: None,
                })
                .await
                .unwrap();
            ledger
                .create_hold(HoldCreateRequest {
                    from: ids[0].clone(),
                    amount_micros: 2_000_000,
                    idem_key: "h1".into(),
                    ref_: None,
                })
                .await
                .unwrap();
            assert_eq!(ledger.receipts().await.len(), count);
        }
        let _ = h;
        // And the frozen log still verifies.
        assert!(ledger
            .verify_receipts(&deterministic_keyring())
            .await
            .is_ok());
    });
}
