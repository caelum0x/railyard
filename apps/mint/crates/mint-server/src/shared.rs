//! Newtype wrappers that let ONE store (and ONE ramp/kyc/payer-key registry) be
//! shared, by `Arc`, across the four domain facades in [`crate::state`].
//!
//! The domain facades (`Ledger<S>`, `StreamLedger<S>`, `ReserveLedger<S,R,K>`,
//! `Aitp402<S,C,K>`) each OWN their generic dependency by value. To share a
//! single backing store/ramp across all of them we wrap an `Arc<dyn _>` in a
//! LOCAL newtype and implement the (foreign) trait for it — the orphan rule
//! permits `impl ForeignTrait for LocalType`. Cloning a wrapper clones only the
//! `Arc`, so every facade drives the same underlying state.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use ed25519_dalek::VerifyingKey;
use mint_core::{
    AccountId, AccountKind, Address, AllowanceId, HoldId, Micros, MintError, StreamId,
};
use mint_protocol::PayerKeys;
use mint_receipts::{Receipt, ReceiptHash, ReceiptLog};
use mint_reserve::{Deposit, KycGate, KycTier, Payout, PayoutDest, PayoutStatus, Ramp, RampError};
use mint_store::{
    AllowanceGrantCommit, AllowanceGrantOutcome, AllowanceRecord, AllowanceRevokeCommit,
    AllowanceSpendCommit, AllowanceSpendOutcome, Balance, CommitOutcome, HoldCaptureCommit,
    HoldCreateCommit, HoldOutcome, HoldRecord, HoldReleaseCommit, LedgerStore, MintCommit,
    MintOutcome, RedeemCommit, RedeemOutcome, StreamCloseCommit, StreamCloseOutcome,
    StreamOpenCommit, StreamOpenOutcome, StreamRecord, StreamTickCommit, StreamTickOutcome,
    TransferCommit,
};
use mint_store::{AccountSummary, PayoutQuery, PayoutRecord, ReceiptQuery, ReserveMovementTotal};
use mint_store::{
    AccountStatus, ApiKeyRecord, DepositReversal, HoldStatus, PayerKeyRecord, ProviderReport,
    ProviderReportLine, ReserveMovement, ReversalStatus, StreamStatus,
};

/// An `Arc`-shared [`LedgerStore`]. Every facade holds a clone; all point at one
/// backing store.
pub struct SharedStore<S: LedgerStore>(pub Arc<S>);

impl<S: LedgerStore> Clone for SharedStore<S> {
    fn clone(&self) -> Self {
        SharedStore(self.0.clone())
    }
}

impl<S: LedgerStore> SharedStore<S> {
    /// Wrap a store in a fresh shared handle.
    pub fn new(store: S) -> Self {
        SharedStore(Arc::new(store))
    }

    /// The raw inner store. Used by operator corrections (a refund /
    /// chargeback claw-back burn); the lifecycle guard itself lives in the
    /// store and is skipped only via `RedeemCommit::bypass_lifecycle`.
    pub fn inner(&self) -> &S {
        &self.0
    }
}

#[async_trait]
impl<S: LedgerStore> LedgerStore for SharedStore<S> {
    async fn create_account(
        &self,
        kind: AccountKind,
        owner_ref: Option<String>,
    ) -> Result<AccountId, MintError> {
        self.0.create_account(kind, owner_ref).await
    }
    async fn create_address(
        &self,
        account_id: &AccountId,
        purpose: Option<String>,
    ) -> Result<Address, MintError> {
        self.0.create_address(account_id, purpose).await
    }
    async fn lookup_address(&self, addr: &Address) -> Result<AccountId, MintError> {
        self.0.lookup_address(addr).await
    }
    // Account lifecycle guard: every store (`PgStore`, `InMemoryStore`) reads
    // the parties' freeze / close state INSIDE the same transaction / critical
    // section as the money write, so an operator freeze can never race an
    // in-flight payment. The claw-back burn opts out explicitly via
    // `RedeemCommit::bypass_lifecycle`.
    async fn commit_transfer(&self, commit: TransferCommit) -> Result<CommitOutcome, MintError> {
        self.0.commit_transfer(commit).await
    }
    async fn commit_hold_create(&self, commit: HoldCreateCommit) -> Result<HoldOutcome, MintError> {
        self.0.commit_hold_create(commit).await
    }
    async fn commit_hold_capture(
        &self,
        commit: HoldCaptureCommit,
    ) -> Result<HoldOutcome, MintError> {
        self.0.commit_hold_capture(commit).await
    }
    async fn commit_hold_release(
        &self,
        commit: HoldReleaseCommit,
    ) -> Result<HoldOutcome, MintError> {
        self.0.commit_hold_release(commit).await
    }
    async fn commit_allowance_grant(
        &self,
        commit: AllowanceGrantCommit,
    ) -> Result<AllowanceGrantOutcome, MintError> {
        self.0.commit_allowance_grant(commit).await
    }
    async fn commit_allowance_spend(
        &self,
        commit: AllowanceSpendCommit,
    ) -> Result<AllowanceSpendOutcome, MintError> {
        self.0.commit_allowance_spend(commit).await
    }
    async fn commit_allowance_revoke(
        &self,
        commit: AllowanceRevokeCommit,
    ) -> Result<AllowanceId, MintError> {
        self.0.commit_allowance_revoke(commit).await
    }
    async fn get_allowance(&self, id: &AllowanceId) -> Result<AllowanceRecord, MintError> {
        self.0.get_allowance(id).await
    }
    async fn commit_stream_open(
        &self,
        commit: StreamOpenCommit,
    ) -> Result<StreamOpenOutcome, MintError> {
        self.0.commit_stream_open(commit).await
    }
    async fn commit_stream_tick(
        &self,
        commit: StreamTickCommit,
    ) -> Result<StreamTickOutcome, MintError> {
        self.0.commit_stream_tick(commit).await
    }
    async fn commit_stream_close(
        &self,
        commit: StreamCloseCommit,
    ) -> Result<StreamCloseOutcome, MintError> {
        self.0.commit_stream_close(commit).await
    }
    async fn get_stream(&self, id: &StreamId) -> Result<StreamRecord, MintError> {
        self.0.get_stream(id).await
    }
    async fn rebuild_streams(&self) -> Result<HashMap<StreamId, StreamRecord>, MintError> {
        self.0.rebuild_streams().await
    }
    async fn commit_mint(&self, commit: MintCommit) -> Result<MintOutcome, MintError> {
        self.0.commit_mint(commit).await
    }
    async fn commit_redeem(&self, commit: RedeemCommit) -> Result<RedeemOutcome, MintError> {
        self.0.commit_redeem(commit).await
    }
    async fn reserve_micros(&self) -> Micros {
        self.0.reserve_micros().await
    }
    async fn get_balance(&self, account_id: &AccountId) -> Result<Balance, MintError> {
        self.0.get_balance(account_id).await
    }
    async fn rebuild_balances(&self) -> Result<HashMap<AccountId, Balance>, MintError> {
        self.0.rebuild_balances().await
    }
    async fn rebuild_holds(&self) -> Result<HashMap<HoldId, HoldRecord>, MintError> {
        self.0.rebuild_holds().await
    }
    async fn journal_len(&self) -> u64 {
        self.0.journal_len().await
    }
    async fn rebuild_receipts(&self) -> Result<ReceiptLog, MintError> {
        self.0.rebuild_receipts().await
    }
    async fn snapshot_receipts(&self) -> Vec<Receipt> {
        self.0.snapshot_receipts().await
    }
    async fn get_receipt(&self, account_id: &AccountId, journal_seq: u64) -> Option<Receipt> {
        self.0.get_receipt(account_id, journal_seq).await
    }
    async fn receipt_merkle_root(&self) -> ReceiptHash {
        self.0.receipt_merkle_root().await
    }
    // Every fallible read MUST be forwarded explicitly: relying on the trait
    // defaults here would bypass `PgStore`'s error-propagating overrides and
    // silently reintroduce the "outage reads as an empty ledger" failure mode.
    async fn try_reserve_micros(&self) -> Result<Micros, MintError> {
        self.0.try_reserve_micros().await
    }
    async fn try_journal_len(&self) -> Result<u64, MintError> {
        self.0.try_journal_len().await
    }
    async fn try_snapshot_receipts(&self) -> Result<Vec<Receipt>, MintError> {
        self.0.try_snapshot_receipts().await
    }
    async fn try_receipt_merkle_root(&self) -> Result<ReceiptHash, MintError> {
        self.0.try_receipt_merkle_root().await
    }
    async fn latest_receipts_per_account(&self) -> Result<Vec<Receipt>, MintError> {
        self.0.latest_receipts_per_account().await
    }
    async fn query_account_receipts(
        &self,
        account_id: &AccountId,
        q: ReceiptQuery,
    ) -> Result<Vec<Receipt>, MintError> {
        self.0.query_account_receipts(account_id, q).await
    }
    async fn account_receipt_before(
        &self,
        account_id: &AccountId,
        ts_ms: i64,
    ) -> Result<Option<Receipt>, MintError> {
        self.0.account_receipt_before(account_id, ts_ms).await
    }
    async fn list_accounts(
        &self,
        after: Option<AccountId>,
        limit: usize,
    ) -> Result<Vec<AccountSummary>, MintError> {
        self.0.list_accounts(after, limit).await
    }
    async fn upsert_payout(&self, record: PayoutRecord) -> Result<PayoutRecord, MintError> {
        self.0.upsert_payout(record).await
    }
    async fn get_payout(&self, payout_ref: &str) -> Result<Option<PayoutRecord>, MintError> {
        self.0.get_payout(payout_ref).await
    }
    async fn list_payouts(&self, q: PayoutQuery) -> Result<Vec<PayoutRecord>, MintError> {
        self.0.list_payouts(q).await
    }
    async fn reserve_movement_totals(&self) -> Result<Vec<ReserveMovementTotal>, MintError> {
        self.0.reserve_movement_totals().await
    }
    async fn list_reserve_movements(
        &self,
        provider: &str,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
        limit: usize,
    ) -> Result<Vec<ReserveMovement>, MintError> {
        self.0.list_reserve_movements(provider, from_ms, to_ms, limit).await
    }
    async fn get_account(&self, account_id: &AccountId) -> Result<AccountSummary, MintError> {
        self.0.get_account(account_id).await
    }
    async fn set_account_status(
        &self,
        account_id: &AccountId,
        status: AccountStatus,
    ) -> Result<AccountSummary, MintError> {
        self.0.set_account_status(account_id, status).await
    }
    async fn get_hold(&self, id: &HoldId) -> Result<HoldRecord, MintError> {
        self.0.get_hold(id).await
    }
    async fn list_account_holds(
        &self,
        account_id: &AccountId,
        status: Option<HoldStatus>,
        limit: usize,
    ) -> Result<Vec<(HoldRecord, Option<i64>)>, MintError> {
        self.0.list_account_holds(account_id, status, limit).await
    }
    async fn list_account_streams(
        &self,
        account_id: &AccountId,
        status: Option<StreamStatus>,
        limit: usize,
    ) -> Result<Vec<StreamRecord>, MintError> {
        self.0.list_account_streams(account_id, status, limit).await
    }
    async fn list_account_allowances(
        &self,
        account_id: &AccountId,
        include_revoked: bool,
        limit: usize,
    ) -> Result<Vec<AllowanceRecord>, MintError> {
        self.0
            .list_account_allowances(account_id, include_revoked, limit)
            .await
    }
    async fn set_hold_expiry(&self, hold_id: &HoldId, expires_at_ms: i64) -> Result<(), MintError> {
        self.0.set_hold_expiry(hold_id, expires_at_ms).await
    }
    async fn hold_expiry(&self, hold_id: &HoldId) -> Result<Option<i64>, MintError> {
        self.0.hold_expiry(hold_id).await
    }
    async fn list_expired_holds(&self, now_ms: i64, limit: usize) -> Result<Vec<HoldId>, MintError> {
        self.0.list_expired_holds(now_ms, limit).await
    }
    async fn insert_payer_key(&self, record: PayerKeyRecord) -> Result<PayerKeyRecord, MintError> {
        self.0.insert_payer_key(record).await
    }
    async fn list_payer_keys(&self, payer: Option<&str>) -> Result<Vec<PayerKeyRecord>, MintError> {
        self.0.list_payer_keys(payer).await
    }
    async fn revoke_payer_key(
        &self,
        payer: &str,
        pubkey_id: &str,
    ) -> Result<Option<PayerKeyRecord>, MintError> {
        self.0.revoke_payer_key(payer, pubkey_id).await
    }
    async fn insert_api_key(&self, record: ApiKeyRecord) -> Result<ApiKeyRecord, MintError> {
        self.0.insert_api_key(record).await
    }
    async fn list_api_keys(&self) -> Result<Vec<ApiKeyRecord>, MintError> {
        self.0.list_api_keys().await
    }
    async fn find_api_key(&self, key_hash: &str) -> Result<Option<ApiKeyRecord>, MintError> {
        self.0.find_api_key(key_hash).await
    }
    async fn revoke_api_key(&self, id: &str) -> Result<Option<ApiKeyRecord>, MintError> {
        self.0.revoke_api_key(id).await
    }
    async fn insert_provider_report(
        &self,
        report: ProviderReport,
        lines: Vec<ProviderReportLine>,
    ) -> Result<ProviderReport, MintError> {
        self.0.insert_provider_report(report, lines).await
    }
    async fn list_provider_reports(&self, limit: usize) -> Result<Vec<ProviderReport>, MintError> {
        self.0.list_provider_reports(limit).await
    }
    async fn get_provider_report(
        &self,
        id: &str,
    ) -> Result<Option<(ProviderReport, Vec<ProviderReportLine>)>, MintError> {
        self.0.get_provider_report(id).await
    }
    async fn delete_provider_report(&self, id: &str) -> Result<bool, MintError> {
        self.0.delete_provider_report(id).await
    }
    async fn upsert_deposit_reversal(
        &self,
        record: DepositReversal,
    ) -> Result<DepositReversal, MintError> {
        self.0.upsert_deposit_reversal(record).await
    }
    async fn get_deposit_reversal(
        &self,
        deposit_ref: &str,
    ) -> Result<Option<DepositReversal>, MintError> {
        self.0.get_deposit_reversal(deposit_ref).await
    }
    async fn list_deposit_reversals(
        &self,
        status: Option<ReversalStatus>,
        limit: usize,
    ) -> Result<Vec<DepositReversal>, MintError> {
        self.0.list_deposit_reversals(status, limit).await
    }
}

/// An `Arc`-shared [`Ramp`] (fiat/crypto on/off-ramp seam).
#[derive(Clone)]
pub struct DynRamp(pub Arc<dyn Ramp>);

#[async_trait]
impl Ramp for DynRamp {
    async fn confirm_deposit(&self, deposit_ref: &str) -> Result<Deposit, RampError> {
        self.0.confirm_deposit(deposit_ref).await
    }
    async fn confirm_deposit_for(
        &self,
        deposit_ref: &str,
        account: &str,
        amount: Micros,
    ) -> Result<Deposit, RampError> {
        self.0.confirm_deposit_for(deposit_ref, account, amount).await
    }
    async fn initiate_payout(
        &self,
        payout_ref: &str,
        dest: &PayoutDest,
        amount: Micros,
    ) -> Result<Payout, RampError> {
        self.0.initiate_payout(payout_ref, dest, amount).await
    }
    async fn check_payout(
        &self,
        payout_ref: &str,
        dest: &PayoutDest,
        amount: Micros,
    ) -> Result<(), RampError> {
        self.0.check_payout(payout_ref, dest, amount).await
    }
    async fn payout_status(
        &self,
        payout_ref: &str,
        provider_ref: Option<&str>,
    ) -> Result<PayoutStatus, RampError> {
        self.0.payout_status(payout_ref, provider_ref).await
    }
}

/// An `Arc`-shared [`KycGate`].
#[derive(Clone)]
pub struct DynKyc(pub Arc<dyn KycGate>);

#[async_trait]
impl KycGate for DynKyc {
    async fn authorize(
        &self,
        account: &AccountId,
        tier: KycTier,
        amount: Micros,
    ) -> Result<(), MintError> {
        self.0.authorize(account, tier, amount).await
    }
}

/// An `Arc`-shared [`PayerKeys`] registry (AITP-402 payer verifying keys).
#[derive(Clone)]
pub struct DynPayerKeys(pub Arc<dyn PayerKeys>);

impl PayerKeys for DynPayerKeys {
    fn verifying_key(&self, payer: &str, pubkey_id: &str) -> Option<VerifyingKey> {
        self.0.verifying_key(payer, pubkey_id)
    }
}
