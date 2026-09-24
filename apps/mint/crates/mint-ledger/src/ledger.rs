//! `Ledger<S>` — the thin, store-agnostic public API.
//!
//! Validates high-level intents at the boundary (amount > 0, no self-transfer),
//! computes the canonical `op_hash`, and delegates the atomic double-entry
//! commit to `store.commit_transfer`. It holds NO locking/persistence logic and
//! NO holds/streams/receipts/AITP-402 (later phases).

use mint_core::{
    op_hash, AccountId, AccountKind, Address, AllowanceId, Micros, MintError, TransferAmount,
};
use mint_receipts::{verify_log, Keyring, Receipt, ReceiptHash, VerifyError};
use mint_store::{
    AllowanceGrantCommit, AllowanceRecord, AllowanceRevokeCommit, AllowanceSpendCommit, Balance,
    HoldCaptureCommit, HoldCreateCommit, HoldOutcome, HoldReleaseCommit, LedgerStore,
    TransferCommit,
};
use serde_json::json;

use crate::allowance::{AllowanceGrantRequest, AllowanceSpendRequest, SpendReceipt};
use crate::hold::{HoldCaptureRequest, HoldCreateRequest, HoldReceipt, HoldReleaseRequest};
use crate::transfer::{TransferReceipt, TransferRequest};

/// Orchestration facade generic over any [`LedgerStore`].
pub struct Ledger<S: LedgerStore> {
    store: S,
}

impl<S: LedgerStore> Ledger<S> {
    /// Wrap a store (e.g. `InMemoryStore` in AP0).
    pub fn new(store: S) -> Self {
        Ledger { store }
    }

    /// Borrow the underlying store (handy for seeding / rebuild in tests).
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Create a new account of `kind`.
    pub async fn create_account(
        &self,
        kind: AccountKind,
        owner_ref: Option<String>,
    ) -> Result<AccountId, MintError> {
        self.store.create_account(kind, owner_ref).await
    }

    /// Create a stable receive address for an account.
    pub async fn create_address(
        &self,
        account_id: &AccountId,
        purpose: Option<String>,
    ) -> Result<Address, MintError> {
        self.store.create_address(account_id, purpose).await
    }

    /// Read an account's live balance.
    pub async fn read_balance(&self, account_id: &AccountId) -> Result<Balance, MintError> {
        self.store.get_balance(account_id).await
    }

    /// Export the full signed receipt log in global append order (for offline
    /// verification / audit).
    pub async fn receipts(&self) -> Vec<Receipt> {
        self.store.snapshot_receipts().await
    }

    /// Resolve the signed receipt emitted for one account at an exact journal
    /// sequence. This is the response-safe lookup for a just-committed or
    /// idempotently replayed operation.
    pub async fn receipt(&self, account_id: &AccountId, journal_seq: u64) -> Option<Receipt> {
        self.store.get_receipt(account_id, journal_seq).await
    }

    /// The Merkle audit root over the whole receipt log (§6 audit).
    pub async fn merkle_root(&self) -> ReceiptHash {
        self.store.receipt_merkle_root().await
    }

    /// Offline-verify the entire receipt history against a public-key-only
    /// [`Keyring`]: per-account hash chain, contiguity, and Ed25519 signatures.
    pub async fn verify_receipts(&self, keyring: &Keyring) -> Result<(), VerifyError> {
        verify_log(&self.store.snapshot_receipts().await, keyring)
    }

    /// Validate and settle a transfer.
    ///
    /// - amount must be strictly positive ([`MintError::InvalidAmount`]);
    /// - `from` and `to` must differ ([`MintError::BadAccountId`]);
    /// - a repeated `idem_key` with an identical body replays the original
    ///   receipt (no-op, `idempotent_replay = true`);
    /// - a repeated `idem_key` with a different body is
    ///   [`MintError::IdempotencyConflict`].
    pub async fn transfer(&self, req: TransferRequest) -> Result<TransferReceipt, MintError> {
        // Boundary validation: amount > 0.
        let amount = TransferAmount::new(Micros::new(req.amount_micros))?;

        // Reject self-transfers (would double-count a single account).
        if req.from == req.to {
            return Err(MintError::BadAccountId);
        }

        // Canonical op hash over the full intent (field-order independent).
        let hash = op_hash(&json!({
            "op": "transfer",
            "from": req.from.as_str(),
            "to": req.to.as_str(),
            "amount_micros": amount.get(),
            "idem_key": req.idem_key,
            "ref": req.ref_,
        }));

        let outcome = self
            .store
            .commit_transfer(TransferCommit {
                from: req.from,
                to: req.to,
                amount,
                idem_key: req.idem_key,
                op_hash: hash,
                ref_: req.ref_,
            })
            .await?;

        Ok(TransferReceipt {
            first_seq: outcome.first_seq,
            from_balance: outcome.from_balance,
            to_balance: outcome.to_balance,
            idempotent_replay: outcome.idempotent_replay,
        })
    }

    /// Reserve `amount` from `from` (available drops, held rises), opening a new
    /// hold. The `op_hash` deliberately EXCLUDES the hold_id (the store mints
    /// it), so an idempotent replay returns the SAME original hold rather than
    /// minting a new one.
    ///
    /// - amount must be strictly positive ([`MintError::InvalidAmount`]);
    /// - `from` must have sufficient available balance
    ///   ([`MintError::InsufficientFunds`]);
    /// - a repeated `idem_key` with an identical body replays the original
    ///   receipt (no-op); a different body is [`MintError::IdempotencyConflict`].
    pub async fn create_hold(&self, req: HoldCreateRequest) -> Result<HoldReceipt, MintError> {
        let amount = TransferAmount::new(Micros::new(req.amount_micros))?;
        let hash = op_hash(&json!({
            "op": "hold.create",
            "from": req.from.as_str(),
            "amount_micros": amount.get(),
            "idem_key": req.idem_key,
            "ref": req.ref_,
        }));
        let outcome = self
            .store
            .commit_hold_create(HoldCreateCommit {
                from: req.from,
                amount,
                idem_key: req.idem_key,
                op_hash: hash,
                ref_: req.ref_,
                expires_at_ms: req.expires_at_ms,
            })
            .await?;
        Ok(receipt_from(outcome))
    }

    /// Settle up to the held amount of a hold to `to`, releasing the remainder
    /// back to the holder and closing the hold `Captured`.
    ///
    /// - amount must be strictly positive ([`MintError::InvalidAmount`]);
    /// - `to == holder` is rejected in-store ([`MintError::BadAccountId`]);
    /// - `amount > held` is [`MintError::CaptureExceedsHold`];
    /// - a distinct close of an already-terminal hold is
    ///   [`MintError::HoldNotFound`]; an identical replay is a no-op.
    pub async fn capture_hold(&self, req: HoldCaptureRequest) -> Result<HoldReceipt, MintError> {
        let amount = TransferAmount::new(Micros::new(req.amount_micros))?;
        let hash = op_hash(&json!({
            "op": "hold.capture",
            "hold_id": req.hold_id.as_str(),
            "amount_micros": amount.get(),
            "to": req.to.as_str(),
            "idem_key": req.idem_key,
            "ref": req.ref_,
        }));
        let outcome = self
            .store
            .commit_hold_capture(HoldCaptureCommit {
                hold_id: req.hold_id,
                amount,
                to: req.to,
                idem_key: req.idem_key,
                op_hash: hash,
                ref_: req.ref_,
            })
            .await?;
        Ok(receipt_from(outcome))
    }

    /// Free the whole hold with NO settlement (ADR-0002 success-only failure
    /// path — "the agent pays nothing on failure"). Closes the hold `Released`;
    /// no payee balance is touched.
    ///
    /// - a distinct close of an already-terminal hold is
    ///   [`MintError::HoldNotFound`]; an identical replay is a no-op.
    pub async fn release_hold(&self, req: HoldReleaseRequest) -> Result<HoldReceipt, MintError> {
        let hash = op_hash(&json!({
            "op": "hold.release",
            "hold_id": req.hold_id.as_str(),
            "idem_key": req.idem_key,
            "ref": req.ref_,
        }));
        let outcome = self
            .store
            .commit_hold_release(HoldReleaseCommit {
                hold_id: req.hold_id,
                idem_key: req.idem_key,
                op_hash: hash,
                ref_: req.ref_,
            })
            .await?;
        Ok(receipt_from(outcome))
    }

    /// Grant a delegated spend budget (SPEC §4). Returns the minted
    /// [`AllowanceId`]. The `op_hash` EXCLUDES the id (the store mints it), so an
    /// idempotent replay returns the SAME original allowance.
    ///
    /// - `limit_micros` must be strictly positive ([`MintError::InvalidAmount`]);
    /// - `granter` must exist ([`MintError::AccountNotFound`]);
    /// - a repeated `idem_key` with a different body is
    ///   [`MintError::IdempotencyConflict`].
    pub async fn grant_allowance(
        &self,
        req: AllowanceGrantRequest,
    ) -> Result<AllowanceId, MintError> {
        let limit = TransferAmount::new(Micros::new(req.limit_micros))?;
        let hash = op_hash(&json!({
            "op": "allowance.grant",
            "granter": req.granter.as_str(),
            "to_key": req.to_key,
            "limit_micros": limit.get(),
            "window": req.window,
            "scope": req.scope,
            "idem_key": req.idem_key,
        }));
        let outcome = self
            .store
            .commit_allowance_grant(AllowanceGrantCommit {
                granter: req.granter,
                to_key: req.to_key,
                limit,
                window: req.window,
                scope: req.scope,
                idem_key: req.idem_key,
                op_hash: hash,
            })
            .await?;
        Ok(outcome.allowance_id)
    }

    /// Spend against an allowance (SPEC §4). Auto-settles by transfer or hold and
    /// bumps the budget ATOMICALLY with the money move.
    ///
    /// - `amount_micros` must be strictly positive ([`MintError::InvalidAmount`]);
    /// - a revoked allowance, an out-of-scope target, or a spend that would
    ///   exceed the remaining budget is [`MintError::AllowanceExceeded`] with NO
    ///   money moved;
    /// - an unknown allowance is [`MintError::AllowanceNotFound`];
    /// - a repeated `idem_key` with an identical body replays the original
    ///   receipt (no money moves twice).
    pub async fn spend_allowance(
        &self,
        req: AllowanceSpendRequest,
    ) -> Result<SpendReceipt, MintError> {
        let amount = TransferAmount::new(Micros::new(req.amount_micros))?;
        let hash = op_hash(&json!({
            "op": "allowance.spend",
            "allowance_id": req.allowance_id.as_str(),
            "amount_micros": amount.get(),
            "to": req.to.as_str(),
            "scheme": req.scheme,
            "ref": req.ref_,
            "idem_key": req.idem_key,
        }));
        let outcome = self
            .store
            .commit_allowance_spend(AllowanceSpendCommit {
                allowance_id: req.allowance_id,
                amount,
                to: req.to,
                scheme: req.scheme,
                ref_: req.ref_,
                idem_key: req.idem_key,
                op_hash: hash,
            })
            .await?;
        Ok(SpendReceipt {
            allowance_id: outcome.allowance_id,
            remaining_micros: outcome.remaining_micros,
            settlement_seq: outcome.settlement_seq,
            hold_id: outcome.hold_id,
            idempotent_replay: outcome.idempotent_replay,
        })
    }

    /// Revoke an allowance so every further spend fails
    /// [`MintError::AllowanceExceeded`]. Naturally idempotent.
    pub async fn revoke_allowance(
        &self,
        allowance_id: AllowanceId,
        idem_key: impl Into<String>,
    ) -> Result<(), MintError> {
        let idem_key = idem_key.into();
        let hash = op_hash(&json!({
            "op": "allowance.revoke",
            "allowance_id": allowance_id.as_str(),
            "idem_key": idem_key,
        }));
        self.store
            .commit_allowance_revoke(AllowanceRevokeCommit {
                allowance_id,
                idem_key,
                op_hash: hash,
            })
            .await?;
        Ok(())
    }

    /// Read a live allowance record (budget, window, scope, revoked state).
    pub async fn read_allowance(&self, id: &AllowanceId) -> Result<AllowanceRecord, MintError> {
        self.store.get_allowance(id).await
    }
}

/// Project a store [`HoldOutcome`] onto the boundary [`HoldReceipt`].
fn receipt_from(outcome: HoldOutcome) -> HoldReceipt {
    HoldReceipt {
        hold_id: outcome.hold_id,
        first_seq: outcome.first_seq,
        holder_balance: outcome.holder_balance,
        payee_balance: outcome.payee_balance,
        status: outcome.status,
        captured_micros: outcome.captured_micros,
        idempotent_replay: outcome.idempotent_replay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mint_store::InMemoryStore;

    fn ledger() -> Ledger<InMemoryStore> {
        Ledger::new(InMemoryStore::new())
    }

    fn req(from: &AccountId, to: &AccountId, amount: i64, key: &str) -> TransferRequest {
        TransferRequest {
            from: from.clone(),
            to: to.clone(),
            amount_micros: amount,
            idem_key: key.to_string(),
            ref_: None,
        }
    }

    #[tokio::test]
    async fn transfer_rejects_non_positive_amount() {
        let l = ledger();
        let a = l.create_account(AccountKind::User, None).await.unwrap();
        let b = l.create_account(AccountKind::User, None).await.unwrap();
        assert_eq!(
            l.transfer(req(&a, &b, 0, "k")).await.unwrap_err(),
            MintError::InvalidAmount
        );
        assert_eq!(
            l.transfer(req(&a, &b, -5, "k")).await.unwrap_err(),
            MintError::InvalidAmount
        );
    }

    #[tokio::test]
    async fn transfer_rejects_self_transfer() {
        let l = ledger();
        let a = l.create_account(AccountKind::User, None).await.unwrap();
        l.store()
            .seed_account(&a, Micros::new(1_000_000))
            .await
            .unwrap();
        assert_eq!(
            l.transfer(req(&a, &a, 1, "k")).await.unwrap_err(),
            MintError::BadAccountId
        );
    }

    #[tokio::test]
    async fn happy_transfer_moves_money() {
        let l = ledger();
        let a = l.create_account(AccountKind::User, None).await.unwrap();
        let b = l.create_account(AccountKind::Model, None).await.unwrap();
        l.store()
            .seed_account(&a, Micros::new(10_000_000))
            .await
            .unwrap();

        let receipt = l.transfer(req(&a, &b, 4_200_000, "k1")).await.unwrap();
        assert!(!receipt.idempotent_replay);
        assert_eq!(
            l.read_balance(&a).await.unwrap().available_micros,
            Micros::new(5_800_000)
        );
        assert_eq!(
            l.read_balance(&b).await.unwrap().available_micros,
            Micros::new(4_200_000)
        );
    }

    #[tokio::test]
    async fn identical_replay_returns_original_receipt() {
        let l = ledger();
        let a = l.create_account(AccountKind::User, None).await.unwrap();
        let b = l.create_account(AccountKind::User, None).await.unwrap();
        l.store()
            .seed_account(&a, Micros::new(10_000_000))
            .await
            .unwrap();

        let first = l.transfer(req(&a, &b, 3_000_000, "k1")).await.unwrap();
        let replay = l.transfer(req(&a, &b, 3_000_000, "k1")).await.unwrap();
        assert!(replay.idempotent_replay);
        assert_eq!(first.first_seq, replay.first_seq);
        assert_eq!(first.from_balance, replay.from_balance);
    }

    #[tokio::test]
    async fn same_key_different_amount_conflicts() {
        let l = ledger();
        let a = l.create_account(AccountKind::User, None).await.unwrap();
        let b = l.create_account(AccountKind::User, None).await.unwrap();
        l.store()
            .seed_account(&a, Micros::new(10_000_000))
            .await
            .unwrap();
        l.transfer(req(&a, &b, 3_000_000, "k1")).await.unwrap();
        assert_eq!(
            l.transfer(req(&a, &b, 7_000_000, "k1")).await.unwrap_err(),
            MintError::IdempotencyConflict
        );
    }

    // ---- AP1: hold boundary tests -----------------------------------------

    use crate::hold::{HoldCaptureRequest, HoldCreateRequest, HoldReleaseRequest};

    fn create_req(from: &AccountId, amount: i64, key: &str) -> HoldCreateRequest {
        HoldCreateRequest {
            from: from.clone(),
            amount_micros: amount,
            idem_key: key.to_string(),
            ref_: None,
            expires_at_ms: None,
        }
    }

    async fn seeded(l: &Ledger<InMemoryStore>, kind: AccountKind, seed: i64) -> AccountId {
        let id = l.create_account(kind, None).await.unwrap();
        if seed != 0 {
            l.store()
                .seed_account(&id, Micros::new(seed))
                .await
                .unwrap();
        }
        id
    }

    #[tokio::test]
    async fn create_hold_rejects_non_positive_amount() {
        let l = ledger();
        let a = seeded(&l, AccountKind::User, 1_000_000).await;
        assert_eq!(
            l.create_hold(create_req(&a, 0, "h")).await.unwrap_err(),
            MintError::InvalidAmount
        );
        assert_eq!(
            l.create_hold(create_req(&a, -5, "h")).await.unwrap_err(),
            MintError::InvalidAmount
        );
    }

    #[tokio::test]
    async fn capture_hold_rejects_non_positive_amount() {
        let l = ledger();
        let a = seeded(&l, AccountKind::User, 10_000_000).await;
        let b = seeded(&l, AccountKind::Model, 0).await;
        let h = l.create_hold(create_req(&a, 4_000_000, "h")).await.unwrap();
        assert_eq!(
            l.capture_hold(HoldCaptureRequest {
                hold_id: h.hold_id.clone(),
                amount_micros: 0,
                to: b.clone(),
                idem_key: "c".into(),
                ref_: None,
            })
            .await
            .unwrap_err(),
            MintError::InvalidAmount
        );
    }

    #[tokio::test]
    async fn create_then_capture_happy_path_balances() {
        let l = ledger();
        let a = seeded(&l, AccountKind::User, 10_000_000).await;
        let b = seeded(&l, AccountKind::Model, 0).await;

        let h = l
            .create_hold(create_req(&a, 4_000_000, "h1"))
            .await
            .unwrap();
        assert_eq!(
            l.read_balance(&a).await.unwrap().available_micros,
            Micros::new(6_000_000)
        );
        assert_eq!(
            l.read_balance(&a).await.unwrap().held_micros,
            Micros::new(4_000_000)
        );

        let cap = l
            .capture_hold(HoldCaptureRequest {
                hold_id: h.hold_id.clone(),
                amount_micros: 2_500_000,
                to: b.clone(),
                idem_key: "c1".into(),
                ref_: None,
            })
            .await
            .unwrap();
        assert_eq!(cap.status, mint_store::HoldStatus::Captured);
        assert_eq!(cap.captured_micros, Micros::new(2_500_000));
        // holder: available back up by the remainder (1,500,000), held cleared.
        let ba = l.read_balance(&a).await.unwrap();
        assert_eq!(ba.held_micros, Micros::zero());
        assert_eq!(ba.available_micros, Micros::new(7_500_000));
        assert_eq!(
            l.read_balance(&b).await.unwrap().available_micros,
            Micros::new(2_500_000)
        );
    }

    #[tokio::test]
    async fn create_then_release_pays_nothing() {
        let l = ledger();
        let a = seeded(&l, AccountKind::User, 10_000_000).await;
        let b = seeded(&l, AccountKind::Model, 0).await;
        let c = seeded(&l, AccountKind::User, 3_000_000).await;

        let h = l
            .create_hold(create_req(&a, 4_000_000, "h1"))
            .await
            .unwrap();
        let rel = l
            .release_hold(HoldReleaseRequest {
                hold_id: h.hold_id.clone(),
                idem_key: "r1".into(),
                ref_: None,
            })
            .await
            .unwrap();
        assert_eq!(rel.status, mint_store::HoldStatus::Released);
        assert!(rel.payee_balance.is_none());

        // Holder gets the WHOLE amount back; every non-holder is untouched.
        assert_eq!(
            l.read_balance(&a).await.unwrap().available_micros,
            Micros::new(10_000_000)
        );
        assert_eq!(
            l.read_balance(&a).await.unwrap().held_micros,
            Micros::zero()
        );
        assert_eq!(
            l.read_balance(&b).await.unwrap().available_micros,
            Micros::zero()
        );
        assert_eq!(
            l.read_balance(&c).await.unwrap().available_micros,
            Micros::new(3_000_000)
        );
    }

    // ---- AP3: allowance boundary tests ------------------------------------

    use crate::allowance::{AllowanceGrantRequest, AllowanceSpendRequest};
    use mint_store::{AllowanceScheme, AllowanceScope, AllowanceWindow, HoldStatus};

    fn grant(granter: &AccountId, limit: i64, key: &str) -> AllowanceGrantRequest {
        AllowanceGrantRequest {
            granter: granter.clone(),
            to_key: "delegate-1".into(),
            limit_micros: limit,
            window: AllowanceWindow::PerCall,
            scope: None,
            idem_key: key.into(),
        }
    }

    fn spend(id: &AllowanceId, amount: i64, to: &AccountId, key: &str) -> AllowanceSpendRequest {
        AllowanceSpendRequest {
            allowance_id: id.clone(),
            amount_micros: amount,
            to: to.clone(),
            scheme: AllowanceScheme::Transfer,
            ref_: None,
            idem_key: key.into(),
        }
    }

    #[tokio::test]
    async fn grant_rejects_non_positive_limit() {
        let l = ledger();
        let g = seeded(&l, AccountKind::User, 10_000_000).await;
        assert_eq!(
            l.grant_allowance(grant(&g, 0, "gk")).await.unwrap_err(),
            MintError::InvalidAmount
        );
    }

    #[tokio::test]
    async fn grant_then_spend_within_budget_moves_money() {
        let l = ledger();
        let g = seeded(&l, AccountKind::User, 10_000_000).await;
        let payee = seeded(&l, AccountKind::Model, 0).await;
        let id = l
            .grant_allowance(grant(&g, 10_000_000, "gk"))
            .await
            .unwrap();

        let r1 = l
            .spend_allowance(spend(&id, 4_000_000, &payee, "s1"))
            .await
            .unwrap();
        assert_eq!(r1.remaining_micros, Micros::new(6_000_000));
        let r2 = l
            .spend_allowance(spend(&id, 5_000_000, &payee, "s2"))
            .await
            .unwrap();
        assert_eq!(r2.remaining_micros, Micros::new(1_000_000));

        // Granter debited 9, payee credited 9, via genuine transfers.
        assert_eq!(
            l.read_balance(&g).await.unwrap().available_micros,
            Micros::new(1_000_000)
        );
        assert_eq!(
            l.read_balance(&payee).await.unwrap().available_micros,
            Micros::new(9_000_000)
        );
    }

    #[tokio::test]
    async fn spend_exceeding_budget_is_rejected_atomically() {
        let l = ledger();
        let g = seeded(&l, AccountKind::User, 10_000_000).await;
        let payee = seeded(&l, AccountKind::Model, 0).await;
        let id = l.grant_allowance(grant(&g, 5_000_000, "gk")).await.unwrap();

        l.spend_allowance(spend(&id, 3_000_000, &payee, "s1"))
            .await
            .unwrap();
        let err = l
            .spend_allowance(spend(&id, 3_000_000, &payee, "s2"))
            .await
            .unwrap_err();
        assert_eq!(err, MintError::AllowanceExceeded);

        // The rejected spend committed NO transfer: payee still holds only 3.
        assert_eq!(
            l.read_balance(&payee).await.unwrap().available_micros,
            Micros::new(3_000_000)
        );
        // And the spent counter is unchanged (remaining still 2).
        let rec = l.read_allowance(&id).await.unwrap();
        assert_eq!(rec.spent_micros, Micros::new(3_000_000));
    }

    #[tokio::test]
    async fn rolling_window_resets_spent_after_elapsed() {
        let clock = std::sync::Arc::new(mint_store::FixedClock::new(1_000_000));
        let l = Ledger::new(InMemoryStore::with_clock(clock.clone()));
        let g = l.create_account(AccountKind::User, None).await.unwrap();
        l.store()
            .seed_account(&g, Micros::new(10_000_000))
            .await
            .unwrap();
        let payee = l.create_account(AccountKind::Model, None).await.unwrap();
        l.store()
            .seed_account(&payee, Micros::new(0))
            .await
            .unwrap();

        let id = l
            .grant_allowance(AllowanceGrantRequest {
                granter: g.clone(),
                to_key: "d".into(),
                limit_micros: 5_000_000,
                window: AllowanceWindow::Rolling { window_ms: 1_000 },
                scope: None,
                idem_key: "gk".into(),
            })
            .await
            .unwrap();

        l.spend_allowance(spend(&id, 5_000_000, &payee, "s1"))
            .await
            .unwrap();
        // At limit: another spend fails within the same window.
        assert_eq!(
            l.spend_allowance(spend(&id, 1, &payee, "s2"))
                .await
                .unwrap_err(),
            MintError::AllowanceExceeded
        );
        // Advance past the window: the budget resets and the spend succeeds.
        clock.advance(1_000);
        let r = l
            .spend_allowance(spend(&id, 4_000_000, &payee, "s3"))
            .await
            .unwrap();
        assert_eq!(r.remaining_micros, Micros::new(1_000_000));
    }

    #[tokio::test]
    async fn scope_mismatch_is_rejected() {
        let l = ledger();
        let g = seeded(&l, AccountKind::User, 10_000_000).await;
        let allowed = seeded(&l, AccountKind::Model, 0).await;
        let other = seeded(&l, AccountKind::Model, 0).await;
        let id = l
            .grant_allowance(AllowanceGrantRequest {
                granter: g.clone(),
                to_key: "d".into(),
                limit_micros: 10_000_000,
                window: AllowanceWindow::PerCall,
                scope: Some(AllowanceScope {
                    payee: Some(allowed.as_str().to_string()),
                    resource_prefix: Some("job:".into()),
                }),
                idem_key: "gk".into(),
            })
            .await
            .unwrap();

        // Wrong payee.
        assert_eq!(
            l.spend_allowance(AllowanceSpendRequest {
                allowance_id: id.clone(),
                amount_micros: 1_000_000,
                to: other.clone(),
                scheme: AllowanceScheme::Transfer,
                ref_: Some("job:1".into()),
                idem_key: "s1".into(),
            })
            .await
            .unwrap_err(),
            MintError::AllowanceExceeded
        );
        // Right payee but wrong resource prefix.
        assert_eq!(
            l.spend_allowance(AllowanceSpendRequest {
                allowance_id: id.clone(),
                amount_micros: 1_000_000,
                to: allowed.clone(),
                scheme: AllowanceScheme::Transfer,
                ref_: Some("nope:1".into()),
                idem_key: "s2".into(),
            })
            .await
            .unwrap_err(),
            MintError::AllowanceExceeded
        );
        // Both correct: succeeds.
        let r = l
            .spend_allowance(AllowanceSpendRequest {
                allowance_id: id.clone(),
                amount_micros: 1_000_000,
                to: allowed.clone(),
                scheme: AllowanceScheme::Transfer,
                ref_: Some("job:42".into()),
                idem_key: "s3".into(),
            })
            .await
            .unwrap();
        assert_eq!(r.remaining_micros, Micros::new(9_000_000));
    }

    #[tokio::test]
    async fn spend_with_hold_scheme_reserves_against_granter() {
        let l = ledger();
        let g = seeded(&l, AccountKind::User, 10_000_000).await;
        let payee = seeded(&l, AccountKind::Model, 0).await;
        let id = l
            .grant_allowance(grant(&g, 10_000_000, "gk"))
            .await
            .unwrap();

        let r = l
            .spend_allowance(AllowanceSpendRequest {
                allowance_id: id.clone(),
                amount_micros: 4_000_000,
                to: payee.clone(),
                scheme: AllowanceScheme::Hold,
                ref_: None,
                idem_key: "s1".into(),
            })
            .await
            .unwrap();
        let hold_id = r.hold_id.expect("hold scheme mints a hold");

        // Granter's available dropped, held rose — nothing credited to payee yet.
        let gb = l.read_balance(&g).await.unwrap();
        assert_eq!(gb.available_micros, Micros::new(6_000_000));
        assert_eq!(gb.held_micros, Micros::new(4_000_000));
        assert_eq!(
            l.read_balance(&payee).await.unwrap().available_micros,
            Micros::zero()
        );

        // The hold is capturable later (proves the AP1 build-on).
        let cap = l
            .capture_hold(HoldCaptureRequest {
                hold_id,
                amount_micros: 4_000_000,
                to: payee.clone(),
                idem_key: "c1".into(),
                ref_: None,
            })
            .await
            .unwrap();
        assert_eq!(cap.status, HoldStatus::Captured);
        assert_eq!(
            l.read_balance(&payee).await.unwrap().available_micros,
            Micros::new(4_000_000)
        );
    }

    #[tokio::test]
    async fn revoked_allowance_cannot_spend() {
        let l = ledger();
        let g = seeded(&l, AccountKind::User, 10_000_000).await;
        let payee = seeded(&l, AccountKind::Model, 0).await;
        let id = l
            .grant_allowance(grant(&g, 10_000_000, "gk"))
            .await
            .unwrap();
        l.revoke_allowance(id.clone(), "rk").await.unwrap();
        assert_eq!(
            l.spend_allowance(spend(&id, 1_000_000, &payee, "s1"))
                .await
                .unwrap_err(),
            MintError::AllowanceExceeded
        );
    }

    #[tokio::test]
    async fn idempotent_spend_moves_money_once() {
        let l = ledger();
        let g = seeded(&l, AccountKind::User, 10_000_000).await;
        let payee = seeded(&l, AccountKind::Model, 0).await;
        let id = l
            .grant_allowance(grant(&g, 10_000_000, "gk"))
            .await
            .unwrap();

        let first = l
            .spend_allowance(spend(&id, 3_000_000, &payee, "s1"))
            .await
            .unwrap();
        assert!(!first.idempotent_replay);
        for _ in 0..3 {
            let replay = l
                .spend_allowance(spend(&id, 3_000_000, &payee, "s1"))
                .await
                .unwrap();
            assert!(replay.idempotent_replay);
            assert_eq!(replay.remaining_micros, first.remaining_micros);
        }
        // Money moved exactly once.
        assert_eq!(
            l.read_balance(&payee).await.unwrap().available_micros,
            Micros::new(3_000_000)
        );
    }

    #[tokio::test]
    async fn spend_on_unknown_allowance_is_not_found() {
        let l = ledger();
        let payee = seeded(&l, AccountKind::Model, 0).await;
        let ghost = AllowanceId::new(ulid::Ulid::new());
        assert_eq!(
            l.spend_allowance(spend(&ghost, 1, &payee, "s1"))
                .await
                .unwrap_err(),
            MintError::AllowanceNotFound
        );
    }
}
