use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use turnstile_core::{
    reconcile_once, AmountMicros, FakeVerifier, HoldRef, InMemorySettlementJournal, PayError,
    PaymentAuthorization, PaymentVerifier, Receipt, Scheme, SettlementJournal, SettlementPhase,
    TurnstileError, VerifiedHold,
};

struct FailFirstCapture {
    captures: AtomicUsize,
    releases: AtomicUsize,
}

#[async_trait]
impl PaymentVerifier for FailFirstCapture {
    async fn verify(&self, _auth: &PaymentAuthorization) -> Result<VerifiedHold, TurnstileError> {
        unreachable!("test starts from capture_pending")
    }

    async fn capture(
        &self,
        _hold: &HoldRef,
        actual: AmountMicros,
    ) -> Result<Receipt, TurnstileError> {
        let attempt = self.captures.fetch_add(1, Ordering::SeqCst);
        if attempt == 0 {
            return Err(TurnstileError::Pay(PayError::Gateway(
                "capture response lost".into(),
            )));
        }
        Ok(Receipt {
            seq: 1,
            op: "hold.capture".into(),
            op_hash: String::new(),
            amount_micros: actual,
            balance_after_micros: AmountMicros::ZERO,
            prev_hash: String::new(),
            hash: String::new(),
            sig: String::new(),
        })
    }

    async fn release(&self, _hold: &HoldRef) -> Result<(), TurnstileError> {
        self.releases.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

fn authorization(nonce: &str, scheme: Scheme) -> PaymentAuthorization {
    PaymentAuthorization {
        payer: "acct_payer".into(),
        payee: "ait1qpayee".into(),
        amount_micros: AmountMicros(1_000),
        nonce: nonce.into(),
        resource: "POST /v1/tool".into(),
        expiry: i64::MAX,
        scheme,
        idempotency_key: format!("aitp:{nonce}"),
        pubkey_id: "key_1".into(),
        op_hash: String::new(),
        sig: String::new(),
    }
}

#[tokio::test]
async fn abandoned_held_request_is_released_without_running_a_handler() {
    let journal = Arc::new(InMemorySettlementJournal::new());
    let verifier = Arc::new(FakeVerifier::with_hold("hold_abandoned"));
    let auth = authorization("nonce_abandoned", Scheme::Hold);
    journal
        .begin_verification(&auth, "request", 0)
        .await
        .unwrap();
    journal
        .bind_verified(
            &auth.nonce,
            "request",
            Some(&HoldRef("hold_abandoned".into())),
            Scheme::Hold,
            0,
        )
        .await
        .unwrap();

    let report = reconcile_once(
        journal.clone(),
        verifier.clone(),
        "reaper",
        Duration::from_secs(30),
        10,
    )
    .await
    .unwrap();

    assert_eq!(report.claimed, 1);
    assert_eq!(report.completed, 1);
    assert_eq!(report.failed, 0);
    assert_eq!(verifier.snapshot().released, vec!["hold_abandoned"]);
    assert!(verifier.snapshot().captured.is_empty());
    assert_eq!(
        journal.get(&auth.nonce).await.unwrap().unwrap().phase,
        SettlementPhase::Released
    );
}

#[tokio::test]
async fn ambiguous_capture_recovery_can_only_replay_capture() {
    let journal = Arc::new(InMemorySettlementJournal::new());
    let verifier = Arc::new(FakeVerifier::with_hold("hold_capture"));
    let auth = authorization("nonce_capture", Scheme::Hold);
    journal
        .begin_verification(&auth, "request", 0)
        .await
        .unwrap();
    journal
        .bind_verified(
            &auth.nonce,
            "request",
            Some(&HoldRef("hold_capture".into())),
            Scheme::Hold,
            0,
        )
        .await
        .unwrap();
    journal
        .prepare_capture(&auth.nonce, "request", AmountMicros(375), 0)
        .await
        .unwrap();
    journal
        .record_error(&auth.nonce, "request", "response lost")
        .await
        .unwrap();

    reconcile_once(
        journal.clone(),
        verifier.clone(),
        "reaper",
        Duration::from_secs(30),
        10,
    )
    .await
    .unwrap();

    let calls = verifier.snapshot();
    assert_eq!(calls.captured, vec![("hold_capture".into(), 375)]);
    assert!(calls.released.is_empty());
    let record = journal.get(&auth.nonce).await.unwrap().unwrap();
    assert_eq!(record.phase, SettlementPhase::Captured);
    assert_eq!(record.receipt.unwrap().amount_micros, AmountMicros(375));
}

#[tokio::test]
async fn interrupted_verification_is_replayed_then_abandoned_hold_is_released() {
    let journal = Arc::new(InMemorySettlementJournal::new());
    let verifier = Arc::new(FakeVerifier::with_hold("hold_verified_on_replay"));
    let auth = authorization("nonce_verifying", Scheme::Hold);
    journal
        .begin_verification(&auth, "request", 0)
        .await
        .unwrap();
    journal
        .record_error(&auth.nonce, "request", "verify response lost")
        .await
        .unwrap();

    reconcile_once(
        journal.clone(),
        verifier.clone(),
        "reaper",
        Duration::from_secs(30),
        10,
    )
    .await
    .unwrap();

    let calls = verifier.snapshot();
    assert_eq!(calls.verified, 1);
    assert_eq!(calls.released, vec!["hold_verified_on_replay"]);
    assert!(calls.captured.is_empty());
    assert_eq!(
        journal.get(&auth.nonce).await.unwrap().unwrap().phase,
        SettlementPhase::Released
    );
}

#[tokio::test]
async fn capture_failure_stays_capture_pending_until_same_command_succeeds() {
    let journal = Arc::new(InMemorySettlementJournal::new());
    let verifier = Arc::new(FailFirstCapture {
        captures: AtomicUsize::new(0),
        releases: AtomicUsize::new(0),
    });
    let auth = authorization("nonce_flaky_capture", Scheme::Hold);
    journal
        .begin_verification(&auth, "request", 0)
        .await
        .unwrap();
    journal
        .bind_verified(
            &auth.nonce,
            "request",
            Some(&HoldRef("hold_flaky".into())),
            Scheme::Hold,
            0,
        )
        .await
        .unwrap();
    journal
        .prepare_capture(&auth.nonce, "request", AmountMicros(600), 0)
        .await
        .unwrap();

    let first = reconcile_once(
        journal.clone(),
        verifier.clone(),
        "reaper_one",
        Duration::ZERO,
        10,
    )
    .await
    .unwrap();
    assert_eq!(first.failed, 1);
    assert_eq!(
        journal.get(&auth.nonce).await.unwrap().unwrap().phase,
        SettlementPhase::CapturePending
    );
    assert_eq!(verifier.releases.load(Ordering::SeqCst), 0);

    let second = reconcile_once(
        journal.clone(),
        verifier.clone(),
        "reaper_two",
        Duration::ZERO,
        10,
    )
    .await
    .unwrap();
    assert_eq!(second.completed, 1);
    assert_eq!(verifier.captures.load(Ordering::SeqCst), 2);
    assert_eq!(verifier.releases.load(Ordering::SeqCst), 0);
    assert_eq!(
        journal.get(&auth.nonce).await.unwrap().unwrap().phase,
        SettlementPhase::Captured
    );
}

#[tokio::test]
async fn stale_owner_cannot_mutate_a_claimed_settlement() {
    let journal = Arc::new(InMemorySettlementJournal::new());
    let auth = authorization("nonce_ownership", Scheme::Hold);
    journal
        .begin_verification(&auth, "request", 0)
        .await
        .unwrap();
    journal
        .bind_verified(
            &auth.nonce,
            "request",
            Some(&HoldRef("hold_owned".into())),
            Scheme::Hold,
            0,
        )
        .await
        .unwrap();

    let claimed = journal
        .claim_recoverable("reaper", 1, i64::MAX, 10)
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert!(journal
        .prepare_capture(&auth.nonce, "request", AmountMicros(500), i64::MAX)
        .await
        .is_err());
    assert!(journal
        .claim_recoverable("second_reaper", 2, i64::MAX, 10)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        journal.get(&auth.nonce).await.unwrap().unwrap().phase,
        SettlementPhase::Held
    );
}
