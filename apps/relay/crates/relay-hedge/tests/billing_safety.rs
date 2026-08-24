//! Billing-safety proof: a hedged race returns exactly ONE winner, so exactly
//! ONE settle reaches the ledger and losers are never charged. Uses the REAL
//! `relay_billing::Ledger` over its `InMemoryStore` (the same reserve -> settle
//! engine the gateway's success-only billing middleware drives).

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{fake_attempt, Script};
use googletest::prelude::*;
use relay_billing::{
    AccountId, InMemoryStore, JobId, Ledger, LedgerEntry, LedgerKind, LedgerStore, Micros, ModelId,
    Usage,
};
use relay_hedge::{race, HedgeConfig};

async fn seed_account(store: &InMemoryStore, account: &AccountId, micros: i64) {
    let deposit = LedgerEntry::new(
        account.clone(),
        LedgerKind::Deposit,
        Micros::new(micros),
        "test_deposit",
        None,
    );
    store
        .append_ledger(vec![deposit])
        .await
        .expect("seed deposit");
}

#[gtest]
#[tokio::test(start_paused = true)]
async fn winner_settles_exactly_once_and_losers_are_never_charged() {
    let store = InMemoryStore::new();
    let ledger = Ledger::new(Arc::new(store.clone()));
    let account = AccountId::new("acct-1");
    seed_account(&store, &account, 1_000_000).await;

    // HOLD before the handler (mirrors the billing middleware's pre-handler
    // reserve of the worst-case estimate).
    let job = JobId::new();
    let reservation = ledger
        .reserve(&account, Micros::new(100_000), job)
        .await
        .expect("reserve hold");

    // Race two provider attempts; the loser hangs and must be canceled.
    let (slow, hs) = fake_attempt("slow", 0, Duration::from_secs(3600), Script::Ok);
    let (fast, hf) = fake_attempt("fast", 0, Duration::from_millis(5), Script::Ok);
    let out = race(vec![slow, fast], &HedgeConfig::hedged(2))
        .await
        .expect("a winner");

    expect_that!(out.winner.as_ref(), eq("fast"));
    expect_that!(hf.completed(), eq(1));
    // The loser was dropped (canceled) and produced no response/usage.
    expect_that!(hs.dropped_incomplete(), eq(true));
    expect_that!(hs.completed(), eq(0));

    // ONLY the winner's usage reaches billing. Settle it once.
    let winner_usage = Usage::new(10, 20, 30, Micros::new(4_200));
    ledger
        .settle(&reservation, winner_usage, ModelId::new("gpt-x"))
        .await
        .expect("settle winner");

    // A repeated settle (idempotency) must not double-charge; and there is no
    // separate settle for the loser because it never produced usage.
    ledger
        .settle(
            &reservation,
            Usage::new(10, 20, 30, Micros::new(4_200)),
            ModelId::new("gpt-x"),
        )
        .await
        .expect("idempotent settle");

    // Exactly one Debit landed in the ledger.
    let entries = store
        .ledger_for_account(&account)
        .await
        .expect("read ledger");
    let debits = entries
        .iter()
        .filter(|e| e.kind == LedgerKind::Debit)
        .count();
    expect_that!(debits, eq(1));

    // Net balance == deposit - actual winner cost (the 100_000 hold was released).
    let balance = ledger.available_micros(&account).await.expect("balance");
    expect_that!(balance.get(), eq(1_000_000 - 4_200));
}

#[gtest]
#[tokio::test(start_paused = true)]
async fn all_providers_failing_release_charges_zero() {
    // When every attempt fails, the race returns an error, the middleware would
    // RELEASE (never settle), and the account nets exactly zero.
    let store = InMemoryStore::new();
    let ledger = Ledger::new(Arc::new(store.clone()));
    let account = AccountId::new("acct-2");
    seed_account(&store, &account, 1_000_000).await;

    let job = JobId::new();
    let reservation = ledger
        .reserve(&account, Micros::new(100_000), job)
        .await
        .expect("reserve hold");

    let (a, _) = fake_attempt("A", 0, Duration::from_millis(1), Script::Err);
    let (b, _) = fake_attempt("B", 0, Duration::from_millis(1), Script::Err);
    let result = race(vec![a, b], &HedgeConfig::hedged(2)).await;
    expect_that!(result.is_err(), eq(true));

    // No usage => release, not settle.
    ledger.release(&reservation).await.expect("release hold");

    let entries = store
        .ledger_for_account(&account)
        .await
        .expect("read ledger");
    let debits = entries
        .iter()
        .filter(|e| e.kind == LedgerKind::Debit)
        .count();
    expect_that!(debits, eq(0));
    let balance = ledger.available_micros(&account).await.expect("balance");
    expect_that!(balance.get(), eq(1_000_000));
}
