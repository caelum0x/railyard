//! Property tests (proptest): random transfer storms preserve every AP0
//! invariant — conservation, double-entry, non-negativity, and the
//! rebuild-from-journal == live-projection gate.

mod common;

use common::*;
use proptest::prelude::*;

/// A single random step: (from index, to index, amount µ$).
fn step_strategy(n: usize) -> impl Strategy<Value = (usize, usize, i64)> {
    (0..n, 0..n, 1i64..2_000_000)
}

fn storm_strategy() -> impl Strategy<Value = (Vec<i64>, Vec<(usize, usize, i64)>)> {
    // 2..=6 accounts, random openings, then up to 150 random transfers.
    prop::collection::vec(0i64..1_000_000, 2..=6).prop_flat_map(|openings| {
        let n = openings.len();
        let steps = prop::collection::vec(step_strategy(n), 0..150);
        (Just(openings), steps)
    })
}

fn run_storm(openings: Vec<i64>, steps: Vec<(usize, usize, i64)>) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async move {
        let ledger = deterministic_ledger();
        let (ids, opening_total) = seed_accounts(&ledger, &openings).await;

        let mut stats = StormStats::default();
        for (i, (from_i, to_i, amount)) in steps.iter().enumerate() {
            apply_step(
                &ledger,
                &mut stats,
                &ids[*from_i],
                &ids[*to_i],
                *amount,
                format!("op-{i}"),
            )
            .await;

            // Non-negativity holds at every single step.
            prop_assert!(
                min_available(&ledger, &ids).await >= 0,
                "no account may go negative mid-storm"
            );
        }

        // 1. Conservation: total supply is unchanged by transfers.
        let after = live_total(&ledger, &ids).await;
        prop_assert_eq!(after, opening_total, "conservation of supply");

        // 2. Double-entry: every transfer nets zero; transfer deltas sum to 0.
        let journal = ledger.store().journal_snapshot();
        assert_double_entry(&journal);
        // Journal delta total (seeds + transfers) == seeded opening supply.
        prop_assert_eq!(journal_delta_total(&journal), opening_total);

        // 3. Gate: rebuild-from-journal == live projection, bit-for-bit.
        assert_rebuild_matches_live(&ledger).await;

        Ok(())
    })
    .unwrap();
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn storm_preserves_all_invariants((openings, steps) in storm_strategy()) {
        run_storm(openings, steps);
    }
}

/// A focused deterministic case: an oversized transfer is rejected with
/// insufficient funds and writes nothing, while the rest of the storm settles.
#[test]
fn oversized_transfer_is_rejected_and_writes_nothing() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    rt.block_on(async {
        let ledger = deterministic_ledger();
        let (ids, total) = seed_accounts(&ledger, &[1_000_000, 0]).await;
        let mut stats = StormStats::default();

        // Way more than the sender holds.
        apply_step(
            &ledger,
            &mut stats,
            &ids[0],
            &ids[1],
            9_999_999,
            "big".into(),
        )
        .await;
        assert_eq!(stats.insufficient, 1);
        assert_eq!(stats.applied, 0);

        // A valid one still settles.
        apply_step(&ledger, &mut stats, &ids[0], &ids[1], 400_000, "ok".into()).await;
        assert_eq!(stats.applied, 1);

        assert_eq!(live_total(&ledger, &ids).await, total);
        assert!(min_available(&ledger, &ids).await >= 0);
        assert_rebuild_matches_live(&ledger).await;
    });
}
