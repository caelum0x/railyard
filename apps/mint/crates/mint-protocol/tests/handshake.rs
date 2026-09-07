//! End-to-end AITP-402 handshake tests: valid transfer/hold settle and emit
//! receipts; replayed nonce, expired auth, and bad signatures are rejected
//! without settling or burning a legitimate nonce; idempotency is exercised via
//! the ledger `idem_key` path.

mod common;

use common::setup;
use mint_ledger::{HoldStatus, LedgerStore};
use mint_protocol::{build_authorization, MintError, Scheme, Settlement};

const TTL: i64 = 60_000;

#[tokio::test]
async fn happy_aitp_transfer_settles_and_emits_receipt() {
    let h = setup().await;
    let challenge = h.proto.issue_challenge_with_nonce(
        "nonce-t1",
        &h.payee_addr,
        4_200_000,
        "POST /v1/chat#llama",
        Scheme::Transfer,
        TTL,
    );
    let auth = build_authorization(h.payer.as_str(), &challenge, "req-t1", &h.payer_signer);

    let settlement = h.proto.verify_and_settle(&auth).await.unwrap();
    match settlement {
        Settlement::Transfer {
            receipt,
            aitp_receipt,
        } => {
            assert!(!receipt.idempotent_replay);
            // The AITP-Receipt header carries the full §6 receipt (payee credit).
            assert_eq!(aitp_receipt.body.amount_micros.get(), 4_200_000);
        }
        _ => panic!("expected a transfer settlement"),
    }

    // Payer debited, provider credited.
    let ledger = h.proto.ledger();
    assert_eq!(
        ledger
            .read_balance(&h.payer)
            .await
            .unwrap()
            .available_micros
            .get(),
        common::PAYER_SEED_MICROS - 4_200_000
    );
    assert_eq!(
        ledger
            .read_balance(&h.provider)
            .await
            .unwrap()
            .available_micros
            .get(),
        4_200_000
    );
}

#[tokio::test]
async fn happy_aitp_hold_then_capture_and_release() {
    let h = setup().await;
    let challenge = h.proto.issue_challenge_with_nonce(
        "nonce-h1",
        &h.payee_addr,
        4_000_000,
        "POST /v1/infer#meter",
        Scheme::Hold,
        TTL,
    );
    let auth = build_authorization(h.payer.as_str(), &challenge, "req-h1", &h.payer_signer);

    let hold_id = match h.proto.verify_and_settle(&auth).await.unwrap() {
        Settlement::Hold {
            hold_id, receipt, ..
        } => {
            assert_eq!(receipt.status, HoldStatus::Open);
            hold_id
        }
        _ => panic!("expected a hold settlement"),
    };

    let ledger = h.proto.ledger();
    // Payer available dropped, held rose.
    let pb = ledger.read_balance(&h.payer).await.unwrap();
    assert_eq!(
        pb.available_micros.get(),
        common::PAYER_SEED_MICROS - 4_000_000
    );
    assert_eq!(pb.held_micros.get(), 4_000_000);

    // Capture the ACTUAL usage (< reserved); remainder is released to the payer.
    let cap = h
        .proto
        .capture(hold_id, 2_500_000, h.provider.clone(), "cap-h1")
        .await
        .unwrap();
    assert_eq!(cap.status, HoldStatus::Captured);
    assert_eq!(
        ledger
            .read_balance(&h.provider)
            .await
            .unwrap()
            .available_micros
            .get(),
        2_500_000
    );
    let pb = ledger.read_balance(&h.payer).await.unwrap();
    assert_eq!(pb.held_micros.get(), 0);
    // Payer paid only the captured 2.5; the 1.5 remainder came back.
    assert_eq!(
        pb.available_micros.get(),
        common::PAYER_SEED_MICROS - 2_500_000
    );

    // A SEPARATE hold that is released leaves the payer whole (pays nothing).
    let c2 = h.proto.issue_challenge_with_nonce(
        "nonce-h2",
        &h.payee_addr,
        1_000_000,
        "POST /v1/infer#meter",
        Scheme::Hold,
        TTL,
    );
    let a2 = build_authorization(h.payer.as_str(), &c2, "req-h2", &h.payer_signer);
    let hold2 = match h.proto.verify_and_settle(&a2).await.unwrap() {
        Settlement::Hold { hold_id, .. } => hold_id,
        _ => panic!("expected hold"),
    };
    let rel = h.proto.release(hold2, "rel-h2").await.unwrap();
    assert_eq!(rel.status, HoldStatus::Released);
    // Payer available is back to exactly post-capture (the released hold cost 0).
    let pb = ledger.read_balance(&h.payer).await.unwrap();
    assert_eq!(pb.held_micros.get(), 0);
    assert_eq!(
        pb.available_micros.get(),
        common::PAYER_SEED_MICROS - 2_500_000
    );
}

#[tokio::test]
async fn exact_authorization_replay_recovers_original_settlement() {
    let h = setup().await;
    let challenge = h.proto.issue_challenge_with_nonce(
        "nonce-r1",
        &h.payee_addr,
        1_000_000,
        "res",
        Scheme::Transfer,
        TTL,
    );
    let auth = build_authorization(h.payer.as_str(), &challenge, "req-r1", &h.payer_signer);

    h.proto.verify_and_settle(&auth).await.unwrap();
    let rows_after_first = h.proto.ledger().store().journal_len().await;

    // Re-presenting the exact signed authorization recovers the original
    // result, allowing a caller to survive response loss without moving money
    // twice.
    let replay = h.proto.verify_and_settle(&auth).await.unwrap();
    match replay {
        Settlement::Transfer { receipt, .. } => assert!(receipt.idempotent_replay),
        _ => panic!("expected transfer replay"),
    }
    // The replay appends no second ledger row.
    assert_eq!(
        h.proto.ledger().store().journal_len().await,
        rows_after_first
    );
}

#[tokio::test]
async fn expired_auth_rejected_without_burning_nonce() {
    let h = setup().await;
    let challenge = h.proto.issue_challenge_with_nonce(
        "nonce-e1",
        &h.payee_addr,
        1_000_000,
        "res",
        Scheme::Transfer,
        TTL,
    );
    let auth = build_authorization(h.payer.as_str(), &challenge, "req-e1", &h.payer_signer);
    let rows_before = h.proto.ledger().store().journal_len().await;

    // Jump past the expiry: rejected, nothing mutated.
    h.clock.set(challenge.expiry + 1);
    assert_eq!(
        h.proto.verify_and_settle(&auth).await.unwrap_err(),
        MintError::AuthorizationExpired
    );
    assert_eq!(h.proto.ledger().store().journal_len().await, rows_before);

    // Rewind before expiry: the SAME auth now settles — proving the earlier
    // expired attempt never claimed (burned) the nonce.
    h.clock.set(challenge.expiry - 1);
    assert!(h.proto.verify_and_settle(&auth).await.is_ok());
}

#[tokio::test]
async fn bad_signature_rejected_without_settling() {
    let h = setup().await;
    let challenge = h.proto.issue_challenge_with_nonce(
        "nonce-b1",
        &h.payee_addr,
        1_000_000,
        "res",
        Scheme::Transfer,
        TTL,
    );
    let rows_before = h.proto.ledger().store().journal_len().await;

    // (a) Tampered signature byte.
    let mut tampered = build_authorization(h.payer.as_str(), &challenge, "req-b1", &h.payer_signer);
    let mut sig = tampered.sig.clone().into_bytes();
    let last = sig.len() - 1;
    sig[last] = if sig[last] == b'A' { b'B' } else { b'A' };
    tampered.sig = String::from_utf8(sig).unwrap();
    assert_eq!(
        h.proto.verify_and_settle(&tampered).await.unwrap_err(),
        MintError::SignatureInvalid
    );

    // (b) Amount mutated AFTER signing → recomputed op_hash mismatches.
    let mut mutated = build_authorization(h.payer.as_str(), &challenge, "req-b1", &h.payer_signer);
    mutated.amount_micros = mint_protocol::Micros::new(1);
    assert_eq!(
        h.proto.verify_and_settle(&mutated).await.unwrap_err(),
        MintError::SignatureInvalid
    );

    // No settlement happened, and the nonce was never burned (a fresh valid auth
    // for the same nonce still settles).
    assert_eq!(h.proto.ledger().store().journal_len().await, rows_before);
    let good = build_authorization(h.payer.as_str(), &challenge, "req-b1", &h.payer_signer);
    assert!(h.proto.verify_and_settle(&good).await.is_ok());
}

#[tokio::test]
async fn externally_issued_authorization_can_settle() {
    // Mint is the authoritative settlement rail, not the owner of Relay's or
    // Turnstile's challenge state. A correctly signed external authorization
    // settles; the protected edge separately enforces its quoted minimum.
    let h = setup().await;
    let challenge = h.proto.issue_challenge_with_nonce(
        "nonce-u1",
        &h.payee_addr,
        5_000_000,
        "res",
        Scheme::Transfer,
        TTL,
    );
    // Forge a challenge-shaped intent with a lower price and self-sign it.
    let cheaper = mint_protocol::Challenge {
        amount_micros: mint_protocol::Micros::new(1),
        ..challenge.clone()
    };
    let auth = build_authorization(h.payer.as_str(), &cheaper, "req-u1", &h.payer_signer);
    assert!(h.proto.verify_and_settle(&auth).await.is_ok());
}

#[tokio::test]
async fn distinct_nonces_reusing_idempotency_key_each_move_money() {
    // Pay-once-use-many guard: two DISTINCT challenges (fresh nonces) that reuse
    // the SAME payer-chosen `idempotency_key` and an identical transfer body must
    // each move money. The settlement idem key is bound to the single-use nonce
    // (`nonce:idempotency_key`), so distinct challenges can never collapse to one
    // ledger op — the second settle is NOT an idempotent replay and the provider
    // is credited for BOTH served requests.
    let h = setup().await;
    let c1 = h.proto.issue_challenge_with_nonce(
        "nonce-i1",
        &h.payee_addr,
        3_000_000,
        "res",
        Scheme::Transfer,
        TTL,
    );
    let c2 = h.proto.issue_challenge_with_nonce(
        "nonce-i2",
        &h.payee_addr,
        3_000_000,
        "res",
        Scheme::Transfer,
        TTL,
    );
    let a1 = build_authorization(h.payer.as_str(), &c1, "same-idem", &h.payer_signer);
    let a2 = build_authorization(h.payer.as_str(), &c2, "same-idem", &h.payer_signer);

    let first = h.proto.verify_and_settle(&a1).await.unwrap();
    let second = h.proto.verify_and_settle(&a2).await.unwrap();
    match (first, second) {
        (Settlement::Transfer { receipt: r1, .. }, Settlement::Transfer { receipt: r2, .. }) => {
            // Neither settle is an idempotent no-op: distinct nonces => distinct
            // ledger ops => real money movement both times.
            assert!(!r1.idempotent_replay);
            assert!(!r2.idempotent_replay);
        }
        _ => panic!("expected two transfer settlements"),
    }

    // Provider credited for BOTH requests (2 x 3_000_000), so the payer cannot
    // serve the second request for free.
    assert_eq!(
        h.proto
            .ledger()
            .read_balance(&h.provider)
            .await
            .unwrap()
            .available_micros
            .get(),
        6_000_000
    );
}
