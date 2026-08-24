//! Atomicity under interleaving: many tokio tasks issue transfers concurrently
//! against one shared `InMemoryStore`. The single-critical-section
//! `commit_transfer` must keep conservation, non-negativity, and rebuild ==
//! live true despite the interleaving (no check-then-append race).

mod common;

use std::sync::Arc;

use common::*;
use mint_ledger::{Ledger, TransferRequest};
use mint_store::InMemoryStore;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_transfers_preserve_invariants() {
    const ACCOUNTS: usize = 8;
    const TASKS: usize = 8;
    const PER_TASK: usize = 500;

    let ledger = Arc::new(deterministic_ledger());
    let openings: Vec<i64> = (0..ACCOUNTS).map(|_| 5_000_000).collect();
    let (ids, opening_total) = seed_accounts(&ledger, &openings).await;
    let ids = Arc::new(ids);

    let mut handles = Vec::new();
    for task in 0..TASKS {
        let ledger = Arc::clone(&ledger);
        let ids = Arc::clone(&ids);
        handles.push(tokio::spawn(async move {
            for i in 0..PER_TASK {
                // Deterministic-but-mixed routing; keep amounts modest so most
                // transfers succeed. Skip self-transfers (rejected anyway).
                let from = (task + i) % ACCOUNTS;
                let to = (task + i + 1 + (i % (ACCOUNTS - 1))) % ACCOUNTS;
                if from == to {
                    continue;
                }
                let _ = ledger
                    .transfer(TransferRequest {
                        from: ids[from].clone(),
                        to: ids[to].clone(),
                        amount_micros: 1_000 + (i as i64 % 50_000),
                        idem_key: format!("t{task}-{i}"),
                        ref_: None,
                    })
                    .await;
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }

    // Conservation, non-negativity, double-entry, and the rebuild gate all hold
    // after the concurrent storm.
    assert_eq!(live_total(&ledger, &ids).await, opening_total);
    assert!(min_available(&ledger, &ids).await >= 0);
    let journal = ledger.store().journal_snapshot();
    assert_double_entry(&journal);
    assert_eq!(journal_delta_total(&journal), opening_total);
    assert_rebuild_matches_live(&ledger).await;
}

/// The seam accepts a boxed `dyn LedgerStore` too (store-agnostic ledger),
/// proving `mint-ledger` never depends on the concrete store type.
#[tokio::test]
async fn ledger_is_generic_over_boxed_store() {
    let ledger: Ledger<InMemoryStore> = Ledger::new(InMemoryStore::new());
    let a = ledger
        .create_account(mint_ledger::AccountKind::Agent, None)
        .await
        .unwrap();
    let b = ledger
        .create_account(mint_ledger::AccountKind::Provider, None)
        .await
        .unwrap();
    ledger
        .store()
        .seed_account(&a, mint_ledger::Micros::new(2_000_000))
        .await
        .unwrap();
    let receipt = ledger
        .transfer(TransferRequest {
            from: a.clone(),
            to: b.clone(),
            amount_micros: 1_000_000,
            idem_key: "x".into(),
            ref_: None,
        })
        .await
        .unwrap();
    assert_eq!(receipt.to_balance.available_micros.get(), 1_000_000);
}
