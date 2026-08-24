//! Property tests (proptest): random storms mixing transfers with
//! create/capture/release preserve every AP1 invariant — conservation over
//! Σ(available + held), double-entry, hold legs net-zero, non-negativity of
//! BOTH legs, and rebuild-from-journal == live projection (incl held) bit-exact.

mod common;

use common::*;
use mint_core::HoldId;
use proptest::prelude::*;

/// One storm step: (kind 0..4, account/hold index i, account index j, amount).
/// kind 0 = transfer, 1 = hold.create, 2 = hold.capture, 3 = hold.release.
type Step = (u8, usize, usize, i64);

fn step_strategy(n: usize) -> impl Strategy<Value = Step> {
    (0u8..4, 0..n, 0..n, 1i64..2_000_000)
}

fn storm_strategy() -> impl Strategy<Value = (Vec<i64>, Vec<Step>)> {
    prop::collection::vec(1i64..1_000_000, 2..=6).prop_flat_map(|openings| {
        let n = openings.len();
        let steps = prop::collection::vec(step_strategy(n), 0..160);
        (Just(openings), steps)
    })
}

/// An open hold tracked at runtime: id, its reserved amount, and holder index.
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
                0 => {
                    apply_step(&ledger, &mut tstats, &ids[*i], &ids[*j], *amount, key).await;
                }
                1 => {
                    if let Some(id) =
                        apply_hold_create(&ledger, &mut hstats, &ids[*i], *amount, key).await
                    {
                        // Only track newly-created (non-replay) holds; replays
                        // can't occur here (unique keys) but guard anyway.
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
                        // A validly-drawn capture: 1..=hold.amount (never > held).
                        let cap = 1 + amount.rem_euclid(h.amount);
                        // Settle to a DIFFERENT account than the holder.
                        let mut to = *j % n;
                        if to == h.holder {
                            to = (to + 1) % n;
                        }
                        let id = h.id.clone();
                        apply_hold_capture(&ledger, &mut hstats, &id, cap, &ids[to], key).await;
                        open.remove(pos); // captured => terminal
                    }
                }
                3 => {
                    if !open.is_empty() {
                        let pos = *i % open.len();
                        let id = open[pos].id.clone();
                        apply_hold_release(&ledger, &mut hstats, &id, key).await;
                        open.remove(pos); // released => terminal
                    }
                }
                _ => unreachable!(),
            }

            // Non-negativity of BOTH legs holds at every single step.
            prop_assert!(
                min_available(&ledger, &ids).await >= 0,
                "no account may go available-negative mid-storm"
            );
            prop_assert!(
                min_held(&ledger, &ids).await >= 0,
                "no account may go held-negative mid-storm"
            );
        }

        // (a) Conservation now spans held: Σ(available + held) is invariant.
        prop_assert_eq!(
            live_total_held(&ledger, &ids).await,
            opening_total,
            "conservation of Σ(available + held)"
        );

        // (b) No validly-drawn capture ever exceeded its hold.
        prop_assert_eq!(hstats.exceeds, 0, "no unexpected CaptureExceedsHold");

        // (d) Double-entry (transfers) + hold legs net to zero.
        let journal = ledger.store().journal_snapshot();
        assert_double_entry(&journal);
        assert_hold_legs_net_zero(&journal);

        // (e) rebuild == live incl held bit-exact + holds table rebuildable.
        assert_rebuild_matches_live(&ledger).await;
        assert_rebuild_holds_matches_live(&ledger).await;

        Ok(())
    })
    .unwrap();
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn mixed_transfer_and_hold_storm_preserves_all_invariants(
        (openings, steps) in storm_strategy()
    ) {
        run_storm(openings, steps);
    }
}
