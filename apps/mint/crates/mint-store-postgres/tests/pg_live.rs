//! Live Postgres tests. `#[ignore]` + gated on `DATABASE_URL`, so they compile
//! (proving the crate uses the sqlx RUNTIME API, not `query!`) but only RUN at
//! deploy against a real database. Run with:
//!
//! ```sh
//! DATABASE_URL=postgres://... cargo test -p mint-store-postgres -- --ignored
//! ```

use std::sync::Arc;

use mint_core::{AccountKind, Micros};
use mint_receipts::FixedSigner;
use mint_store::{LedgerStore, TransferCommit};
use mint_store_postgres::PgStore;

async fn connect() -> Option<PgStore> {
    let url = std::env::var("DATABASE_URL").ok()?;
    let store = PgStore::connect(&url, Arc::new(FixedSigner::new()))
        .await
        .expect("connect");
    store.run_migrations().await.expect("migrations");
    Some(store)
}

#[tokio::test]
#[ignore = "requires a live DATABASE_URL"]
async fn migrations_and_account_lifecycle() {
    let Some(store) = connect().await else {
        return;
    };
    let a = store
        .create_account(AccountKind::User, None)
        .await
        .expect("create account");
    let bal = store.get_balance(&a).await.expect("balance");
    assert_eq!(bal.available_micros, Micros::zero());
    assert_eq!(bal.held_micros, Micros::zero());
}

#[tokio::test]
#[ignore = "requires a live DATABASE_URL"]
async fn transfer_conserves_and_rebuild_matches_live() {
    let Some(store) = connect().await else {
        return;
    };
    let a = store.create_account(AccountKind::User, None).await.unwrap();
    let b = store
        .create_account(AccountKind::Model, None)
        .await
        .unwrap();

    // Fund `a` via a mint (single-sided credit + reserve rise).
    let amount = mint_core::TransferAmount::new(Micros::new(10_000_000)).unwrap();
    let mint_hash = mint_core::op_hash(&serde_json::json!({ "op": "mint", "to": a.as_str() }));
    store
        .commit_mint(mint_store::MintCommit {
            to: a.clone(),
            amount,
            deposit_ref: "dep-1".into(),
            idem_key: "mint:dep-1".into(),
            op_hash: mint_hash,
        })
        .await
        .unwrap();

    let xfer_amount = mint_core::TransferAmount::new(Micros::new(3_000_000)).unwrap();
    let op_hash = mint_core::op_hash(&serde_json::json!({
        "op": "transfer", "from": a.as_str(), "to": b.as_str(), "amount": 3_000_000
    }));
    store
        .commit_transfer(TransferCommit {
            from: a.clone(),
            to: b.clone(),
            amount: xfer_amount,
            idem_key: "xfer-1".into(),
            op_hash,
            ref_: None,
        })
        .await
        .unwrap();

    // AP0 gate: rebuild == live projection.
    let live = store.get_balance(&b).await.unwrap();
    let rebuilt = store.rebuild_balances().await.unwrap();
    assert_eq!(
        rebuilt.get(&b).unwrap().available_micros,
        live.available_micros
    );
    assert_eq!(live.available_micros, Micros::new(3_000_000));
}
