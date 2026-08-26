//! REAL integration tests for the durable Postgres [`NonceStore`].
//!
//! These are the genuine article: they connect to a live Postgres, run the real
//! embedded migration, and drive the real `sqlx` queries. Nothing is mocked.
//!
//! They are compiled only under `--features postgres` and run only when
//! `TURNSTILE_DATABASE_URL` points at a reachable Postgres; otherwise each test
//! prints a SKIP line and returns, so the suite stays green on a machine without
//! a database. To run them:
//!
//! ```bash
//! TURNSTILE_DATABASE_URL=postgres://localhost/turnstile_test \
//!   cargo +1.96.1 test -p turnstile-core --features postgres
//! ```
#![cfg(feature = "postgres")]

use std::sync::Arc;

use turnstile_core::error::TurnstileError;
use turnstile_core::nonce::NonceStore;
use turnstile_core::pg::PostgresNonceStore;

/// Connect + migrate against `TURNSTILE_DATABASE_URL`, or `None` to SKIP.
async fn store() -> Option<PostgresNonceStore> {
    let url = std::env::var("TURNSTILE_DATABASE_URL").ok()?;
    let store = PostgresNonceStore::connect(&url)
        .await
        .expect("connect to TURNSTILE_DATABASE_URL");
    store.migrate().await.expect("run turnstile migrations");
    Some(store)
}

/// A fresh, collision-free nonce per test so runs don't step on each other.
fn unique_nonce(tag: &str) -> String {
    format!("test_{tag}_{:032x}", rand::random::<u128>())
}

#[tokio::test]
async fn register_consume_then_replay_is_rejected() {
    let Some(store) = store().await else {
        eprintln!("SKIP register_consume_then_replay_is_rejected: set TURNSTILE_DATABASE_URL");
        return;
    };
    let nonce = unique_nonce("live");

    store.register(&nonce, 10_000).await.unwrap();
    // First consume within its validity window succeeds.
    assert!(store.consume(&nonce, 500).await.is_ok());
    // The row is gone: a second consume is a replay, not merely a flag flip.
    assert!(matches!(
        store.consume(&nonce, 500).await,
        Err(TurnstileError::NonceReplayed(_))
    ));
}

#[tokio::test]
async fn expired_nonce_is_rejected_on_consume() {
    let Some(store) = store().await else {
        eprintln!("SKIP expired_nonce_is_rejected_on_consume: set TURNSTILE_DATABASE_URL");
        return;
    };
    let nonce = unique_nonce("expiry");

    store.register(&nonce, 100).await.unwrap();
    assert!(matches!(
        store.consume(&nonce, 200).await, // now (200) > expiry (100)
        Err(TurnstileError::NonceExpired)
    ));
}

#[tokio::test]
async fn sweep_reaps_only_expired_rows() {
    let Some(store) = store().await else {
        eprintln!("SKIP sweep_reaps_only_expired_rows: set TURNSTILE_DATABASE_URL");
        return;
    };
    let expired = unique_nonce("sweep_old");
    let live = unique_nonce("sweep_new");

    store.register(&expired, 100).await.unwrap();
    store.register(&live, 10_000).await.unwrap();

    // Sweeping at t=200 drops the expired row (and reports at least it).
    let removed = store.sweep(200).await.unwrap();
    assert!(removed >= 1, "sweep should remove the expired nonce");

    // The expired nonce is gone; the live one still consumes cleanly.
    assert!(matches!(
        store.consume(&expired, 200).await,
        Err(TurnstileError::NonceReplayed(_))
    ));
    assert!(store.consume(&live, 200).await.is_ok());
}

#[tokio::test]
async fn concurrent_consume_admits_exactly_one() {
    let Some(store) = store().await else {
        eprintln!("SKIP concurrent_consume_admits_exactly_one: set TURNSTILE_DATABASE_URL");
        return;
    };
    let store = Arc::new(store);
    let nonce = unique_nonce("race");
    store.register(&nonce, 10_000).await.unwrap();

    // Fan out many racing consumers against the same shared row. The atomic
    // DELETE ... RETURNING must hand the row to exactly one of them.
    let mut handles = Vec::new();
    for _ in 0..16 {
        let store = store.clone();
        let nonce = nonce.clone();
        handles.push(tokio::spawn(async move {
            store.consume(&nonce, 500).await.is_ok()
        }));
    }

    let mut wins = 0;
    for handle in handles {
        if handle.await.unwrap() {
            wins += 1;
        }
    }
    assert_eq!(wins, 1, "exactly one concurrent consumer may win the nonce");
}
