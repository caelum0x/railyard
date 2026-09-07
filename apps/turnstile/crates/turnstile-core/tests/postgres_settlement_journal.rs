//! Live Postgres acceptance for settlement transitions and atomic recovery
//! claims. Tests skip when `TURNSTILE_DATABASE_URL` is not configured.
#![cfg(feature = "postgres")]

use std::sync::Arc;

use turnstile_core::{
    AmountMicros, HoldRef, PaymentAuthorization, PostgresNonceStore, PostgresSettlementJournal,
    Receipt, Scheme, SettlementJournal, SettlementPhase,
};

async fn journal() -> Option<PostgresSettlementJournal> {
    let url = std::env::var("TURNSTILE_DATABASE_URL").ok()?;
    let store = PostgresNonceStore::connect(&url)
        .await
        .expect("connect to TURNSTILE_DATABASE_URL");
    store.migrate().await.expect("run turnstile migrations");
    Some(PostgresSettlementJournal::from_pool(store.pool().clone()))
}

fn authorization(tag: &str) -> PaymentAuthorization {
    let nonce = format!("settlement_{tag}_{:032x}", rand::random::<u128>());
    PaymentAuthorization {
        payer: "acct_payer".into(),
        payee: "ait1qpayee".into(),
        amount_micros: AmountMicros(1_000),
        nonce: nonce.clone(),
        resource: "POST /v1/tool".into(),
        expiry: i64::MAX,
        scheme: Scheme::Hold,
        idempotency_key: format!("aitp:{nonce}"),
        pubkey_id: "key_1".into(),
        op_hash: String::new(),
        sig: String::new(),
    }
}

fn receipt(amount: AmountMicros) -> Receipt {
    Receipt {
        seq: 7,
        op: "hold.capture".into(),
        op_hash: "11".repeat(32),
        amount_micros: amount,
        balance_after_micros: AmountMicros(50_000),
        prev_hash: "22".repeat(32),
        hash: "33".repeat(32),
        sig: "44".repeat(64),
    }
}

#[tokio::test]
async fn capture_command_and_signed_receipt_survive_round_trip() {
    let Some(journal) = journal().await else {
        eprintln!("SKIP capture_command_and_signed_receipt_survive_round_trip");
        return;
    };
    let auth = authorization("round_trip");
    journal
        .begin_verification(&auth, "request", i64::MIN)
        .await
        .unwrap();
    journal
        .bind_verified(
            &auth.nonce,
            "request",
            Some(&HoldRef("hold_pg".into())),
            Scheme::Hold,
            i64::MIN,
        )
        .await
        .unwrap();
    journal
        .prepare_capture(&auth.nonce, "request", AmountMicros(425), i64::MIN)
        .await
        .unwrap();
    let captured = receipt(AmountMicros(425));
    journal
        .complete_capture(&auth.nonce, "request", &captured)
        .await
        .unwrap();

    let stored = journal.get(&auth.nonce).await.unwrap().unwrap();
    assert_eq!(stored.authorization, auth);
    assert_eq!(stored.phase, SettlementPhase::Captured);
    assert_eq!(stored.receipt, Some(captured));
    assert!(journal
        .claim_recoverable("reaper", i64::MIN + 1, i64::MAX, 10)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn concurrent_reapers_claim_an_expired_command_exactly_once() {
    let Some(journal) = journal().await else {
        eprintln!("SKIP concurrent_reapers_claim_an_expired_command_exactly_once");
        return;
    };
    let journal = Arc::new(journal);
    let auth = authorization("claim_race");
    journal
        .begin_verification(&auth, "request", i64::MIN)
        .await
        .unwrap();
    journal
        .bind_verified(
            &auth.nonce,
            "request",
            Some(&HoldRef("hold_claim".into())),
            Scheme::Hold,
            i64::MIN,
        )
        .await
        .unwrap();

    let first = {
        let journal = journal.clone();
        tokio::spawn(async move {
            journal
                .claim_recoverable("reaper_a", i64::MIN + 1, i64::MAX, 1)
                .await
                .unwrap()
                .len()
        })
    };
    let second = {
        let journal = journal.clone();
        tokio::spawn(async move {
            journal
                .claim_recoverable("reaper_b", i64::MIN + 1, i64::MAX, 1)
                .await
                .unwrap()
                .len()
        })
    };

    assert_eq!(first.await.unwrap() + second.await.unwrap(), 1);
}
