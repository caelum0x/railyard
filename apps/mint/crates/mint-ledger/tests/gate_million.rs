//! AP0 acceptance gate: 1,000,000 random transfers must conserve supply, keep
//! double-entry + non-negativity, and rebuild-from-journal == live projection.
//!
//! Ignored by default (it is the heavy gate); run explicitly, ideally release:
//!   cargo test -p mint-ledger --release --test gate_million -- --ignored --nocapture

mod common;

use common::*;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

#[tokio::test]
#[ignore = "1M-transfer acceptance gate; run explicitly (prefer --release)"]
async fn million_transfer_conservation_gate() {
    const ACCOUNTS: usize = 64;
    const STEPS: usize = 1_000_000;

    let ledger = deterministic_ledger();
    let openings: Vec<i64> = (0..ACCOUNTS).map(|i| (i as i64 + 1) * 1_000_000).collect();
    let (ids, opening_total) = seed_accounts(&ledger, &openings).await;

    let mut rng = StdRng::seed_from_u64(0xA170);
    let mut stats = StormStats::default();
    for i in 0..STEPS {
        let from = rng.random_range(0..ACCOUNTS);
        let to = rng.random_range(0..ACCOUNTS);
        let amount = rng.random_range(1i64..2_000_000);
        apply_step(
            &ledger,
            &mut stats,
            &ids[from],
            &ids[to],
            amount,
            format!("m-{i}"),
        )
        .await;
    }

    assert_eq!(
        live_total(&ledger, &ids).await,
        opening_total,
        "conservation after 1M transfers"
    );
    assert!(
        min_available(&ledger, &ids).await >= 0,
        "non-negativity after 1M"
    );
    let journal = ledger.store().journal_snapshot();
    assert_double_entry(&journal);
    assert_eq!(journal_delta_total(&journal), opening_total);
    assert_rebuild_matches_live(&ledger).await;

    eprintln!(
        "1M gate: applied={} insufficient={} invalid={} journal_rows={}",
        stats.applied,
        stats.insufficient,
        stats.self_or_invalid,
        journal.len()
    );
}
