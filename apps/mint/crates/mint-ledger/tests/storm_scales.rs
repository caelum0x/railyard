//! Scaled transfer storms (K ∈ {1k, 10k, 100k}) with a seeded RNG. Each storm
//! asserts conservation, double-entry, non-negativity, and rebuild == live.

mod common;

use common::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

async fn run_scaled_storm(num_accounts: usize, num_steps: usize, seed: u64) {
    let ledger = deterministic_ledger();

    // Deterministic openings: account i seeded with (i+1) * 1_000_000 µ$.
    let openings: Vec<i64> = (0..num_accounts)
        .map(|i| (i as i64 + 1) * 1_000_000)
        .collect();
    let (ids, opening_total) = seed_accounts(&ledger, &openings).await;

    let mut rng = StdRng::seed_from_u64(seed);
    let mut stats = StormStats::default();
    for i in 0..num_steps {
        let from = rng.random_range(0..num_accounts);
        let to = rng.random_range(0..num_accounts);
        // Amounts up to ~1.5× a mid opening so many succeed and some fail.
        let amount = rng.random_range(1i64..3_000_000);
        apply_step(
            &ledger,
            &mut stats,
            &ids[from],
            &ids[to],
            amount,
            format!("s-{i}"),
        )
        .await;
    }

    // Conservation.
    assert_eq!(
        live_total(&ledger, &ids).await,
        opening_total,
        "conservation after {num_steps}-transfer storm"
    );
    // Non-negativity (final).
    assert!(min_available(&ledger, &ids).await >= 0);
    // Double-entry.
    let journal = ledger.store().journal_snapshot();
    assert_double_entry(&journal);
    assert_eq!(journal_delta_total(&journal), opening_total);
    // Gate: rebuild == live.
    assert_rebuild_matches_live(&ledger).await;

    // At least some transfers actually applied (storm did real work).
    assert!(stats.applied > 0, "storm should apply some transfers");
}

#[tokio::test]
async fn storm_1k() {
    run_scaled_storm(8, 1_000, 1).await;
}

#[tokio::test]
async fn storm_10k() {
    run_scaled_storm(16, 10_000, 2).await;
}

#[tokio::test]
async fn storm_100k() {
    run_scaled_storm(32, 100_000, 3).await;
}
