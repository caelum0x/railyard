//! AP4 reserve integration tests: mint creates AIT ONLY against a confirmed
//! injected deposit (and bumps the reserve in lockstep); mint without a deposit
//! is rejected; mint is idempotent per deposit_ref; redeem is KYC/limit-gated
//! and decrements the reserve atomically; the payout is post-commit and
//! retry-safe; and the §6 solvency proof asserts Σ(avail+held) == reserve.

use std::sync::Arc;

use mint_core::{op_hash, AccountId, AccountKind, Micros, MintError, TransferAmount};
use mint_reserve::{
    Asset, FakeKycGate, FakeRamp, KycTier, MintRequest, PayoutDest, PayoutStatus, RedeemRequest,
    ReserveLedger,
};
use mint_store::{FixedClock, InMemoryStore, LedgerStore, TransferCommit};
use serde_json::json;

type RL = ReserveLedger<InMemoryStore, FakeRamp, FakeKycGate>;

fn reserve_ledger(ramp: FakeRamp, kyc: FakeKycGate) -> RL {
    ReserveLedger::new(
        InMemoryStore::with_clock(Arc::new(FixedClock::new(1_700_000_000_000))),
        ramp,
        kyc,
    )
}

async fn account(rl: &RL, kind: AccountKind) -> AccountId {
    rl.store().create_account(kind, None).await.unwrap()
}

fn mint_req(to: &AccountId, amount: i64, deposit_ref: &str) -> MintRequest {
    MintRequest {
        to: to.clone(),
        amount_micros: amount,
        deposit_ref: deposit_ref.into(),
        idem_key: format!("mreq:{deposit_ref}"),
    }
}

fn redeem_req(from: &AccountId, amount: i64, payout_ref: &str, tier: KycTier) -> RedeemRequest {
    RedeemRequest {
        from: from.clone(),
        amount_micros: amount,
        payout_ref: payout_ref.into(),
        kyc_tier: tier,
        payout_dest: PayoutDest::Stripe {
            account: "acct_ext_1".into(),
        },
        idem_key: format!("rreq:{payout_ref}"),
    }
}

#[tokio::test]
async fn mint_binds_credit_to_the_provider_verified_account() {
    // A deposit whose provider-AUTHENTICATED re-query says account `payer` paid
    // must NOT be creditable to any other account, even though the (possibly
    // unauthenticated) webhook could name a different target.
    let store = InMemoryStore::with_clock(Arc::new(FixedClock::new(1_700_000_000_000)));
    let payer = store.create_account(AccountKind::User, None).await.unwrap();
    let attacker = store.create_account(AccountKind::User, None).await.unwrap();
    let ramp = FakeRamp::new().with_bound_deposit("dep", 500, Asset::Usd, payer.as_str());
    let rl = ReserveLedger::new(store, ramp, FakeKycGate::new(KycTier::Verified, i64::MAX));

    // Forged target (attacker) → rejected as an unconfirmed deposit, no credit.
    assert_eq!(
        rl.mint(mint_req(&attacker, 500, "dep")).await.unwrap_err(),
        MintError::DepositNotConfirmed
    );
    assert_eq!(
        rl.store()
            .get_balance(&attacker)
            .await
            .unwrap()
            .available_micros,
        Micros::new(0)
    );

    // The real payer → credited exactly the bound amount.
    let r = rl.mint(mint_req(&payer, 500, "dep")).await.unwrap();
    assert!(!r.idempotent_replay);
    assert_eq!(
        rl.store()
            .get_balance(&payer)
            .await
            .unwrap()
            .available_micros,
        Micros::new(500)
    );
}

#[tokio::test]
async fn mint_only_against_a_confirmed_deposit() {
    let ramp = FakeRamp::new()
        .with_confirmed_deposit("d1", 500, Asset::Usd)
        .with_pending_deposit("d2", 500, Asset::Usdc);
    let kyc = FakeKycGate::new(KycTier::Verified, i64::MAX);
    let rl = reserve_ledger(ramp, kyc);
    let to = account(&rl, AccountKind::User).await;

    // Confirmed deposit → credits available AND reserve by 500.
    let r = rl.mint(mint_req(&to, 500, "d1")).await.unwrap();
    assert!(!r.idempotent_replay);
    assert_eq!(r.reserve_after_micros, Micros::new(500));
    assert_eq!(
        rl.store().get_balance(&to).await.unwrap().available_micros,
        Micros::new(500)
    );
    assert_eq!(rl.store().reserve_micros_snapshot(), Micros::new(500));
    // exactly one Mint receipt appended.
    let snap = rl.store().receipts_snapshot();
    assert_eq!(snap.last().unwrap().body.op, mint_receipts::ReceiptOp::Mint);

    // Unknown deposit → DepositNotConfirmed, NO state change.
    let jbefore = rl.store().journal_len().await;
    assert_eq!(
        rl.mint(mint_req(&to, 100, "unknown")).await.unwrap_err(),
        MintError::DepositNotConfirmed
    );
    // Pending (unconfirmed) deposit → DepositNotConfirmed.
    assert_eq!(
        rl.mint(mint_req(&to, 100, "d2")).await.unwrap_err(),
        MintError::DepositNotConfirmed
    );
    assert_eq!(rl.store().journal_len().await, jbefore);
    assert_eq!(rl.store().reserve_micros_snapshot(), Micros::new(500));
}

#[tokio::test]
async fn mint_amount_over_deposit_is_a_mismatch() {
    let ramp = FakeRamp::new().with_confirmed_deposit("d1", 500, Asset::Usd);
    let rl = reserve_ledger(ramp, FakeKycGate::new(KycTier::None, i64::MAX));
    let to = account(&rl, AccountKind::User).await;
    assert_eq!(
        rl.mint(mint_req(&to, 501, "d1")).await.unwrap_err(),
        MintError::DepositAmountMismatch
    );
    assert_eq!(rl.store().reserve_micros_snapshot(), Micros::zero());
}

#[tokio::test]
async fn mint_is_idempotent_per_deposit_ref() {
    let ramp = FakeRamp::new().with_confirmed_deposit("d1", 500, Asset::Usd);
    let rl = reserve_ledger(ramp, FakeKycGate::new(KycTier::None, i64::MAX));
    let to = account(&rl, AccountKind::User).await;

    let first = rl.mint(mint_req(&to, 500, "d1")).await.unwrap();
    for _ in 0..3 {
        let replay = rl.mint(mint_req(&to, 500, "d1")).await.unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(replay.reserve_after_micros, first.reserve_after_micros);
    }
    // Credited exactly once.
    assert_eq!(
        rl.store().get_balance(&to).await.unwrap().available_micros,
        Micros::new(500)
    );
    assert_eq!(rl.store().reserve_micros_snapshot(), Micros::new(500));

    // Same deposit_ref, different amount → the confirmed deposit already minted.
    assert_eq!(
        rl.mint(mint_req(&to, 400, "d1")).await.unwrap_err(),
        MintError::IdempotencyConflict
    );
}

#[tokio::test]
async fn mint_retry_with_different_caller_idem_key_replays_cleanly() {
    // A legitimate retry reuses the same deposit_ref/amount/target but supplies a
    // FRESH caller idem_key (a common client pattern where the idempotency key is
    // minted per HTTP attempt). Because deposit_ref is the authoritative dedupe
    // key, this must replay cleanly — NOT return IdempotencyConflict.
    let ramp = FakeRamp::new().with_confirmed_deposit("d1", 500, Asset::Usd);
    let rl = reserve_ledger(ramp, FakeKycGate::new(KycTier::None, i64::MAX));
    let to = account(&rl, AccountKind::User).await;

    let first = rl
        .mint(MintRequest {
            to: to.clone(),
            amount_micros: 500,
            deposit_ref: "d1".into(),
            idem_key: "attempt-1".into(),
        })
        .await
        .unwrap();
    assert!(!first.idempotent_replay);

    let replay = rl
        .mint(MintRequest {
            to: to.clone(),
            amount_micros: 500,
            deposit_ref: "d1".into(),
            idem_key: "attempt-2-fresh-key".into(),
        })
        .await
        .unwrap();
    assert!(
        replay.idempotent_replay,
        "a retry with a fresh caller idem_key must replay, not conflict"
    );
    assert_eq!(replay.reserve_after_micros, first.reserve_after_micros);

    // Credited EXACTLY once despite the differing caller idem_key.
    assert_eq!(
        rl.store().get_balance(&to).await.unwrap().available_micros,
        Micros::new(500)
    );
    assert_eq!(rl.store().reserve_micros_snapshot(), Micros::new(500));
}

#[tokio::test]
async fn redeem_retry_with_different_caller_idem_key_replays_cleanly() {
    // The off-ramp mirror of the mint case: retrying redeem with the same
    // payout_ref but a fresh caller idem_key must replay the burn (no double-burn)
    // and re-send the payout exactly once, per redeem()'s documented contract.
    let ramp = FakeRamp::new().with_confirmed_deposit("d1", 1_000, Asset::Usd);
    let kyc = FakeKycGate::new(KycTier::Verified, i64::MAX);
    let rl = reserve_ledger(ramp, kyc);
    let acc = account(&rl, AccountKind::User).await;
    rl.mint(mint_req(&acc, 1_000, "d1")).await.unwrap();

    let first = rl
        .redeem(RedeemRequest {
            from: acc.clone(),
            amount_micros: 400,
            payout_ref: "p1".into(),
            kyc_tier: KycTier::Verified,
            payout_dest: PayoutDest::Stripe {
                account: "acct_ext_1".into(),
            },
            idem_key: "attempt-1".into(),
        })
        .await
        .unwrap();
    assert!(!first.idempotent_replay);
    assert_eq!(first.payout.status, PayoutStatus::Paid);

    let replay = rl
        .redeem(RedeemRequest {
            from: acc.clone(),
            amount_micros: 400,
            payout_ref: "p1".into(),
            kyc_tier: KycTier::Verified,
            payout_dest: PayoutDest::Stripe {
                account: "acct_ext_1".into(),
            },
            idem_key: "attempt-2-fresh-key".into(),
        })
        .await
        .unwrap();
    assert!(
        replay.idempotent_replay,
        "a retry with a fresh caller idem_key must replay the burn, not conflict"
    );

    // Burned exactly once; exactly one payout of 400.
    assert_eq!(
        rl.store().get_balance(&acc).await.unwrap().available_micros,
        Micros::new(600)
    );
    assert_eq!(rl.store().reserve_micros_snapshot(), Micros::new(600));
    let payouts = rl.ramp().payouts();
    assert_eq!(payouts.len(), 1);
    assert_eq!(payouts[0].amount_micros, Micros::new(400));
}

#[tokio::test]
async fn redeem_burns_and_decrements_reserve_records_one_payout() {
    let ramp = FakeRamp::new().with_confirmed_deposit("d1", 1_000, Asset::Usd);
    let kyc = FakeKycGate::new(KycTier::Verified, 500);
    let rl = reserve_ledger(ramp, kyc);
    let acc = account(&rl, AccountKind::User).await;
    rl.mint(mint_req(&acc, 1_000, "d1")).await.unwrap();

    let r = rl
        .redeem(redeem_req(&acc, 200, "p1", KycTier::Verified))
        .await
        .unwrap();
    assert_eq!(r.payout.status, PayoutStatus::Paid);
    assert_eq!(
        rl.store().get_balance(&acc).await.unwrap().available_micros,
        Micros::new(800)
    );
    assert_eq!(rl.store().reserve_micros_snapshot(), Micros::new(800));
    // exactly one payout of 200.
    let payouts = rl.ramp().payouts();
    assert_eq!(payouts.len(), 1);
    assert_eq!(payouts[0].amount_micros, Micros::new(200));

    // Over the per-tx limit (500) → PayoutLimitExceeded, no burn, no payout.
    assert_eq!(
        rl.redeem(redeem_req(&acc, 600, "p2", KycTier::Verified))
            .await
            .unwrap_err(),
        MintError::PayoutLimitExceeded
    );
    // Below required tier → KycRequired.
    assert_eq!(
        rl.redeem(redeem_req(&acc, 100, "p3", KycTier::Basic))
            .await
            .unwrap_err(),
        MintError::KycRequired
    );
    // Reserve unchanged by the rejected redemptions; still exactly one payout.
    assert_eq!(rl.store().reserve_micros_snapshot(), Micros::new(800));
    assert_eq!(rl.ramp().payouts().len(), 1);
}

#[tokio::test]
async fn redeem_over_balance_is_insufficient_funds() {
    let ramp = FakeRamp::new().with_confirmed_deposit("d1", 300, Asset::Usd);
    let kyc = FakeKycGate::new(KycTier::Verified, i64::MAX);
    let rl = reserve_ledger(ramp, kyc);
    let acc = account(&rl, AccountKind::User).await;
    rl.mint(mint_req(&acc, 300, "d1")).await.unwrap();
    assert_eq!(
        rl.redeem(redeem_req(&acc, 400, "p1", KycTier::Verified))
            .await
            .unwrap_err(),
        MintError::InsufficientFunds
    );
    assert_eq!(rl.store().reserve_micros_snapshot(), Micros::new(300));
    assert!(rl.ramp().payouts().is_empty());
}

#[tokio::test]
async fn redeem_below_reserve_backing_is_reserve_insufficient() {
    // seed_account adds available WITHOUT bumping the reserve, so a redeem sees a
    // fundable balance but an under-backed reserve — the defense-in-depth guard.
    let ramp = FakeRamp::new();
    let kyc = FakeKycGate::new(KycTier::Verified, i64::MAX);
    let rl = reserve_ledger(ramp, kyc);
    let acc = account(&rl, AccountKind::User).await;
    rl.store()
        .seed_account(&acc, Micros::new(1_000))
        .await
        .unwrap();

    assert_eq!(
        rl.redeem(redeem_req(&acc, 1_000, "p1", KycTier::Verified))
            .await
            .unwrap_err(),
        MintError::ReserveInsufficient
    );
    // No burn happened.
    assert_eq!(
        rl.store().get_balance(&acc).await.unwrap().available_micros,
        Micros::new(1_000)
    );
}

#[tokio::test]
async fn redeem_payout_failure_is_retry_safe_no_double_burn() {
    let ramp = FakeRamp::new().with_confirmed_deposit("d1", 1_000, Asset::Usd);
    let kyc = FakeKycGate::new(KycTier::Verified, i64::MAX);
    let rl = reserve_ledger(ramp, kyc);
    let acc = account(&rl, AccountKind::User).await;
    rl.mint(mint_req(&acc, 1_000, "d1")).await.unwrap();

    // Simulate a provider outage AFTER the atomic burn.
    rl.ramp().set_payout_failure(true);
    let r1 = rl
        .redeem(redeem_req(&acc, 400, "p1", KycTier::Verified))
        .await
        .unwrap();
    // Supply already burned; payout reported Failed; nothing recorded.
    assert_eq!(r1.payout.status, PayoutStatus::Failed);
    assert_eq!(rl.store().reserve_micros_snapshot(), Micros::new(600));
    assert!(rl.ramp().payouts().is_empty());

    // Recover and retry the SAME payout_ref: no double-burn, exactly one payout.
    rl.ramp().set_payout_failure(false);
    let r2 = rl
        .redeem(redeem_req(&acc, 400, "p1", KycTier::Verified))
        .await
        .unwrap();
    assert!(r2.idempotent_replay); // burn replayed, not repeated
    assert_eq!(r2.payout.status, PayoutStatus::Paid);
    assert_eq!(rl.store().reserve_micros_snapshot(), Micros::new(600)); // still 600
    let payouts = rl.ramp().payouts();
    assert_eq!(payouts.len(), 1);
    assert_eq!(payouts[0].amount_micros, Micros::new(400));
}

#[tokio::test]
async fn solvency_proof_holds_then_catches_a_tamper() {
    let ramp = FakeRamp::new()
        .with_confirmed_deposit("d1", 500, Asset::Usd)
        .with_confirmed_deposit("d2", 300, Asset::Usdc);
    let kyc = FakeKycGate::new(KycTier::Verified, i64::MAX);
    let rl = reserve_ledger(ramp, kyc);
    let a = account(&rl, AccountKind::User).await;
    let b = account(&rl, AccountKind::User).await;
    rl.mint(mint_req(&a, 500, "d1")).await.unwrap();
    rl.mint(mint_req(&b, 300, "d2")).await.unwrap();
    rl.redeem(redeem_req(&a, 100, "p1", KycTier::Verified))
        .await
        .unwrap();

    let proof = rl.solvency().await.unwrap();
    assert!(
        proof.ok,
        "backed {:?} != supply {:?}",
        proof.backed_micros, proof.supply_micros
    );
    assert_eq!(proof.backed_micros, proof.supply_micros);
    assert_eq!(proof.supply_micros, rl.store().reserve_micros_snapshot());

    // Tamper: seed adds available WITHOUT bumping the reserve → shortfall.
    rl.store()
        .seed_account(&b, Micros::new(1_000))
        .await
        .unwrap();
    let bad = rl.solvency().await.unwrap();
    assert!(!bad.ok, "auditor must catch the reserve shortfall");
}

/// Drive a store-level double-entry transfer (the same op the `Ledger` facade
/// emits) directly on the reserve ledger's store, so an in-ledger transfer can
/// be exercised in a solvency scenario without a second store.
async fn transfer(rl: &RL, from: &AccountId, to: &AccountId, amount: i64, idem: &str) {
    let hash = op_hash(&json!({
        "op": "transfer",
        "from": from.as_str(),
        "to": to.as_str(),
        "amount": amount,
    }));
    rl.store()
        .commit_transfer(TransferCommit {
            from: from.clone(),
            to: to.clone(),
            amount: TransferAmount::new(Micros::new(amount)).unwrap(),
            idem_key: idem.into(),
            op_hash: hash,
            ref_: None,
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn solvency_holds_across_mint_transfer_redeem() {
    // The §6 invariant Σ(available + held) == reserve must survive a full
    // in-ledger lifecycle at the facade boundary: a mint bumps the reserve, an
    // internal transfer moves value between accounts (reserve unchanged, backing
    // unchanged), and a redeem burns supply + reserve in lockstep. `ok` stays
    // true throughout, then flips false only under a forced off-book tamper.
    let ramp = FakeRamp::new().with_confirmed_deposit("d1", 1_000, Asset::Usd);
    let kyc = FakeKycGate::new(KycTier::Verified, i64::MAX);
    let rl = reserve_ledger(ramp, kyc);
    let a = account(&rl, AccountKind::User).await;
    let b = account(&rl, AccountKind::User).await;

    // 1. Mint 1_000 to `a`: reserve and backing both == 1_000.
    rl.mint(mint_req(&a, 1_000, "d1")).await.unwrap();
    let after_mint = rl.solvency().await.unwrap();
    assert!(after_mint.ok);
    assert_eq!(after_mint.supply_micros, Micros::new(1_000));
    assert_eq!(after_mint.backed_micros, after_mint.supply_micros);

    // 2. Transfer 400 a -> b: conserves supply (reserve and backing unchanged).
    transfer(&rl, &a, &b, 400, "t1").await;
    assert_eq!(
        rl.store().get_balance(&a).await.unwrap().available_micros,
        Micros::new(600)
    );
    assert_eq!(
        rl.store().get_balance(&b).await.unwrap().available_micros,
        Micros::new(400)
    );
    let after_xfer = rl.solvency().await.unwrap();
    assert!(after_xfer.ok, "a transfer conserves supply — still solvent");
    assert_eq!(after_xfer.supply_micros, Micros::new(1_000));
    assert_eq!(after_xfer.backed_micros, after_xfer.supply_micros);

    // 3. Redeem 250 from b: burn drops available AND reserve in lockstep.
    rl.redeem(redeem_req(&b, 250, "p1", KycTier::Verified))
        .await
        .unwrap();
    let after_redeem = rl.solvency().await.unwrap();
    assert!(
        after_redeem.ok,
        "burn keeps reserve == backing — still solvent"
    );
    assert_eq!(after_redeem.supply_micros, Micros::new(750));
    assert_eq!(after_redeem.backed_micros, after_redeem.supply_micros);
    assert_eq!(
        after_redeem.supply_micros,
        rl.store().reserve_micros_snapshot()
    );

    // 4. Off-book tamper (available up, reserve untouched) => insolvent.
    rl.store().seed_account(&a, Micros::new(500)).await.unwrap();
    let bad = rl.solvency().await.unwrap();
    assert!(!bad.ok, "auditor must catch value credited off the reserve");
}
