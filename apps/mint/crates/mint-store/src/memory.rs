//! `InMemoryStore` — the AP0 [`LedgerStore`] impl.
//!
//! All state lives behind a single `Mutex`. `commit_transfer` takes the guard
//! exactly once, so the idempotency lookup, both-account balance check,
//! non-negative guard, two-row append, and projection apply happen in one
//! critical section — no check-then-append TOCTOU race. This single global lock
//! is the AP0 form of the documented 2-account sorted lock ordering; a future
//! Postgres impl expresses the same atomicity as one SQL transaction with row
//! locks.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use mint_core::{
    seed_bytes16, AccountId, AccountKind, Address, AllowanceId, HoldId, Micros, MintError, OpHash,
    StreamId, MICROS_PER_AIT,
};
use mint_receipts::{
    merkle_root, FixedSigner, Receipt, ReceiptDraft, ReceiptHash, ReceiptLog, ReceiptOp, Signer,
};
use ulid::Ulid;

use crate::balance::Balance;
use crate::clock::{Clock, SystemClock};
use crate::journal::{Bucket, JournalEntry, OpKind};
use crate::operator::{
    AccountStatus, ApiKeyRecord, DepositReversal, PayerKeyRecord, ProviderReport,
    ProviderReportLine, ReserveMovement, ReversalStatus,
};
use crate::reporting::{
    provider_tag_of, AccountSummary, PayoutQuery, PayoutRecord, ReserveMovementTotal,
};
use crate::store_trait::{
    AllowanceGrantCommit, AllowanceGrantOutcome, AllowanceRecord, AllowanceRevokeCommit,
    AllowanceScheme, AllowanceSpendCommit, AllowanceSpendOutcome, AllowanceWindow, CommitOutcome,
    HoldCaptureCommit, HoldCreateCommit, HoldOutcome, HoldRecord, HoldReleaseCommit, HoldStatus,
    LedgerStore, MintCommit, MintOutcome, RedeemCommit, RedeemOutcome, StreamCloseCommit,
    StreamCloseOutcome, StreamOpenCommit, StreamOpenOutcome, StreamRecord, StreamStatus,
    StreamTickCommit, StreamTickOutcome, StreamUnit, TransferCommit,
};

/// The recorded outcome of a committed op, generalized so transfers and holds
/// share ONE idempotency key-space. Replay flips `idempotent_replay` on the
/// stored variant; a same-key/different-body call is an `IdempotencyConflict`.
#[derive(Clone)]
enum StoredOutcome {
    Transfer(CommitOutcome),
    Hold(HoldOutcome),
    AllowanceGrant(AllowanceGrantOutcome),
    AllowanceSpend(AllowanceSpendOutcome),
    StreamOpen(StreamOpenOutcome),
    StreamTick(StreamTickOutcome),
    StreamClose(StreamCloseOutcome),
    Mint(MintOutcome),
    Redeem(RedeemOutcome),
}

/// An idempotency-index entry: the recorded outcome plus the `op_hash` used to
/// detect same-key/different-body conflicts.
#[derive(Clone)]
struct StoredCommit {
    op_hash: OpHash,
    outcome: StoredOutcome,
}

/// The interior state, guarded by one lock.
#[derive(Default)]
struct State {
    journal: Vec<JournalEntry>,
    next_seq: u64,
    balances: HashMap<AccountId, Balance>,
    holds: HashMap<HoldId, HoldRecord>,
    /// Live metered streams (AP4), keyed by minted id. A stream reuses the hold
    /// reservation/settlement legs and is conservation-neutral (never touches the
    /// reserve), so open/tick/close move value only between the available/held
    /// legs of the payer and payee.
    streams: HashMap<StreamId, StreamRecord>,
    /// The in-store reserve ledger (AP4). Mutated ONLY by `commit_mint` (+) and
    /// `commit_redeem` (−), inside the same critical section as the account leg,
    /// so `Σ(available + held) == reserve` holds by construction.
    reserve_micros: Micros,
    /// Live delegated allowances (AP3), keyed by minted id. Auto-settlement of a
    /// spend moves money through the SAME journal/receipt path as a transfer or
    /// hold, so conservation and chain integrity hold across allowance ops too.
    allowances: HashMap<AllowanceId, AllowanceRecord>,
    idem_index: HashMap<String, StoredCommit>,
    addresses: HashMap<Address, AccountId>,
    accounts: HashMap<AccountId, AccountKind>,
    /// The per-account hash-linked, signed receipt log (AP2). Advances lock-step
    /// with the journal inside the same critical section.
    receipts: ReceiptLog,
    /// Opaque owner references + creation times supplied at account creation
    /// (operator listing only; never consulted by money paths).
    account_meta: HashMap<AccountId, (Option<String>, i64)>,
    /// The durable post-burn payout log, keyed by `payout_ref`.
    payouts: HashMap<String, PayoutRecord>,
    /// Operator lifecycle state per account (absent ⇒ `Active`).
    account_status: HashMap<AccountId, AccountStatus>,
    /// DB-backed API keys (hashed secrets only).
    api_keys: Vec<ApiKeyRecord>,
    /// Auto-release deadlines (unix ms) for holds that carry one.
    hold_expiries: HashMap<HoldId, i64>,
    /// Registered AITP-402 payer verifying keys.
    payer_keys: Vec<PayerKeyRecord>,
    /// Imported provider settlement reports with their parsed lines.
    provider_reports: Vec<(ProviderReport, Vec<ProviderReportLine>)>,
    /// Deposit reversals (refund / chargeback claw-backs), keyed by deposit ref.
    reversals: HashMap<String, DepositReversal>,
    /// Set to `true` only while a critical section is mid-write (between its
    /// first and last mutation). If a panic poisons the lock while this is set,
    /// the state is torn and `lock()` refuses to serve it; a panic before any
    /// write leaves this `false`, so pre-write faults recover cleanly.
    writing: bool,
}

impl State {
    /// Refuse an OUTGOING movement from a frozen or closed account. Read inside
    /// the store's single critical section, so an operator freeze can never
    /// interleave between the check and the money write.
    fn ensure_can_send_locked(&self, id: &AccountId) -> Result<(), MintError> {
        match self.account_status.get(id).copied().unwrap_or_default() {
            AccountStatus::Active => Ok(()),
            AccountStatus::Frozen => Err(MintError::AccountFrozen),
            AccountStatus::Closed => Err(MintError::AccountClosed),
        }
    }

    /// Refuse an INCOMING credit to a closed account (a frozen account still
    /// receives, so a customer's deposit is never bounced).
    fn ensure_can_receive_locked(&self, id: &AccountId) -> Result<(), MintError> {
        match self.account_status.get(id).copied().unwrap_or_default() {
            AccountStatus::Closed => Err(MintError::AccountClosed),
            AccountStatus::Active | AccountStatus::Frozen => Ok(()),
        }
    }

    /// The transfer WRITE path — validation + two journal rows + projection +
    /// two receipts — factored out so BOTH `commit_transfer` and an allowance
    /// auto-settlement drive the identical atomic double-entry apply (keeping
    /// conservation and receipt-chain integrity uniform). Idempotency is the
    /// CALLER's job: this never reads or writes `idem_index`. On success it
    /// enters the write phase (`writing = true`); the caller clears it after
    /// recording the outcome.
    #[allow(clippy::too_many_arguments)]
    fn settle_transfer_locked(
        &mut self,
        signer: &dyn Signer,
        from: &AccountId,
        to: &AccountId,
        amount: Micros,
        op_hash: OpHash,
        idem_key: &str,
        ref_: Option<String>,
        ts: i64,
    ) -> Result<CommitOutcome, MintError> {
        // Reject self-transfers: from == to would race two clones of the same
        // balance and last-write-wins, minting money in the projection.
        if from == to {
            return Err(MintError::BadAccountId);
        }
        let from_bal = self
            .balances
            .get(from)
            .cloned()
            .ok_or(MintError::AccountNotFound)?;
        let to_bal = self
            .balances
            .get(to)
            .cloned()
            .ok_or(MintError::AccountNotFound)?;

        let new_from = from_bal.available_micros.checked_sub(amount)?;
        if new_from.is_negative() {
            return Err(MintError::InsufficientFunds);
        }
        let new_to = to_bal.available_micros.checked_add(amount)?;
        let neg_amount = amount.checked_neg()?;

        let debit_seq = self.next_seq;
        let credit_seq = debit_seq + 1;
        // Enter the write phase: every op below is infallible/non-panicking.
        self.writing = true;
        self.journal.push(JournalEntry {
            seq: debit_seq,
            op: OpKind::Transfer,
            op_hash,
            idem_key: idem_key.to_string(),
            account_id: from.clone(),
            counter_id: Some(to.clone()),
            delta_micros: neg_amount,
            bucket: Bucket::Available,
            ref_: ref_.clone(),
            ts,
        });
        self.journal.push(JournalEntry {
            seq: credit_seq,
            op: OpKind::Transfer,
            op_hash,
            idem_key: idem_key.to_string(),
            account_id: to.clone(),
            counter_id: Some(from.clone()),
            delta_micros: amount,
            bucket: Bucket::Available,
            ref_: ref_.clone(),
            ts,
        });
        self.next_seq = credit_seq + 1;

        let from_balance = Balance {
            account_id: from.clone(),
            available_micros: new_from,
            held_micros: from_bal.held_micros,
            updated_seq: debit_seq,
        };
        let to_balance = Balance {
            account_id: to.clone(),
            available_micros: new_to,
            held_micros: to_bal.held_micros,
            updated_seq: credit_seq,
        };
        self.balances.insert(from.clone(), from_balance.clone());
        self.balances.insert(to.clone(), to_balance.clone());

        self.receipts.append(
            ReceiptDraft {
                account_id: from.clone(),
                journal_seq: debit_seq,
                op: ReceiptOp::Transfer,
                op_hash,
                amount_micros: amount,
                from: Some(from.clone()),
                to: Some(to.clone()),
                balance_after_available_micros: from_balance.available_micros,
                balance_after_held_micros: from_balance.held_micros,
                ts,
            },
            signer,
        );
        self.receipts.append(
            ReceiptDraft {
                account_id: to.clone(),
                journal_seq: credit_seq,
                op: ReceiptOp::Transfer,
                op_hash,
                amount_micros: amount,
                from: Some(from.clone()),
                to: Some(to.clone()),
                balance_after_available_micros: to_balance.available_micros,
                balance_after_held_micros: to_balance.held_micros,
                ts,
            },
            signer,
        );

        Ok(CommitOutcome {
            first_seq: debit_seq,
            from_balance,
            to_balance,
            idempotent_replay: false,
        })
    }

    /// The `hold.create` WRITE path — reserve available→held, mint a `HoldId`,
    /// append two legs, open the record, emit one receipt — factored out so both
    /// `commit_hold_create` and an allowance `Hold` auto-settlement mint holds
    /// the same way. Idempotency is the CALLER's job.
    #[allow(clippy::too_many_arguments)]
    fn settle_hold_create_locked(
        &mut self,
        signer: &dyn Signer,
        from: &AccountId,
        amount: Micros,
        op_hash: OpHash,
        idem_key: &str,
        ref_: Option<String>,
        ts: i64,
    ) -> Result<HoldOutcome, MintError> {
        let from_bal = self
            .balances
            .get(from)
            .cloned()
            .ok_or(MintError::AccountNotFound)?;
        let new_available = from_bal.available_micros.checked_sub(amount)?;
        if new_available.is_negative() {
            return Err(MintError::InsufficientFunds);
        }
        let new_held = from_bal.held_micros.checked_add(amount)?;
        let neg_amount = amount.checked_neg()?;

        let hold_id = HoldId::new(Ulid::new());
        let hold_ref = Some(hold_id.as_str().to_string());
        let avail_seq = self.next_seq;
        let held_seq = avail_seq + 1;
        self.writing = true;
        self.journal.push(JournalEntry {
            seq: avail_seq,
            op: OpKind::HoldCreate,
            op_hash,
            idem_key: idem_key.to_string(),
            account_id: from.clone(),
            counter_id: None,
            delta_micros: neg_amount,
            bucket: Bucket::Available,
            ref_: hold_ref.clone(),
            ts,
        });
        self.journal.push(JournalEntry {
            seq: held_seq,
            op: OpKind::HoldCreate,
            op_hash,
            idem_key: idem_key.to_string(),
            account_id: from.clone(),
            counter_id: None,
            delta_micros: amount,
            bucket: Bucket::Held,
            ref_: hold_ref,
            ts,
        });
        self.next_seq = held_seq + 1;

        let holder_balance = Balance {
            account_id: from.clone(),
            available_micros: new_available,
            held_micros: new_held,
            updated_seq: held_seq,
        };
        self.balances.insert(from.clone(), holder_balance.clone());

        let record = HoldRecord {
            id: hold_id.clone(),
            account_id: from.clone(),
            amount_micros: amount,
            captured_micros: Micros::zero(),
            status: HoldStatus::Open,
            ref_: ref_.clone(),
        };
        self.holds.insert(hold_id.clone(), record);

        self.receipts.append(
            ReceiptDraft {
                account_id: from.clone(),
                journal_seq: held_seq,
                op: ReceiptOp::HoldCreate,
                op_hash,
                amount_micros: amount,
                from: Some(from.clone()),
                to: None,
                balance_after_available_micros: holder_balance.available_micros,
                balance_after_held_micros: holder_balance.held_micros,
                ts,
            },
            signer,
        );

        Ok(HoldOutcome {
            hold_id,
            first_seq: avail_seq,
            holder_balance,
            payee_balance: None,
            status: HoldStatus::Open,
            captured_micros: Micros::zero(),
            idempotent_replay: false,
        })
    }

    /// The `stream.open` WRITE path — reserve the whole `cap` available→held on
    /// the payer, mint a `StreamId`, append two legs tagged `OpKind::StreamOpen`,
    /// open the `StreamRecord`, emit one receipt. Mirrors `settle_hold_create`
    /// verbatim for the two legs (conservation-neutral). Idempotency is the
    /// CALLER's job.
    #[allow(clippy::too_many_arguments)]
    fn settle_stream_open_locked(
        &mut self,
        signer: &dyn Signer,
        from: &AccountId,
        to: &AccountId,
        rate: Micros,
        cap: Micros,
        unit: StreamUnit,
        op_hash: OpHash,
        idem_key: &str,
        ref_: Option<String>,
        ts: i64,
    ) -> Result<StreamOpenOutcome, MintError> {
        if from == to {
            return Err(MintError::BadAccountId);
        }
        let from_bal = self
            .balances
            .get(from)
            .cloned()
            .ok_or(MintError::AccountNotFound)?;
        // The payee must exist so ticks can credit it later.
        if !self.balances.contains_key(to) {
            return Err(MintError::AccountNotFound);
        }
        let new_available = from_bal.available_micros.checked_sub(cap)?;
        if new_available.is_negative() {
            return Err(MintError::InsufficientFunds);
        }
        let new_held = from_bal.held_micros.checked_add(cap)?;
        let neg_cap = cap.checked_neg()?;

        let stream_id = StreamId::new(Ulid::new());
        let stream_ref = Some(stream_id.as_str().to_string());
        let avail_seq = self.next_seq;
        let held_seq = avail_seq + 1;
        self.writing = true;
        self.journal.push(JournalEntry {
            seq: avail_seq,
            op: OpKind::StreamOpen,
            op_hash,
            idem_key: idem_key.to_string(),
            account_id: from.clone(),
            counter_id: Some(to.clone()),
            delta_micros: neg_cap,
            bucket: Bucket::Available,
            ref_: stream_ref.clone(),
            ts,
        });
        self.journal.push(JournalEntry {
            seq: held_seq,
            op: OpKind::StreamOpen,
            op_hash,
            idem_key: idem_key.to_string(),
            account_id: from.clone(),
            counter_id: Some(to.clone()),
            delta_micros: cap,
            bucket: Bucket::Held,
            ref_: stream_ref,
            ts,
        });
        self.next_seq = held_seq + 1;

        let holder_balance = Balance {
            account_id: from.clone(),
            available_micros: new_available,
            held_micros: new_held,
            updated_seq: held_seq,
        };
        self.balances.insert(from.clone(), holder_balance.clone());

        let record = StreamRecord {
            id: stream_id.clone(),
            from_id: from.clone(),
            to_id: to.clone(),
            rate_micros: rate,
            unit,
            cap_micros: cap,
            settled_micros: Micros::zero(),
            last_seq: 0,
            status: StreamStatus::Open,
            ref_: ref_.clone(),
        };
        self.streams.insert(stream_id.clone(), record);

        self.receipts.append(
            ReceiptDraft {
                account_id: from.clone(),
                journal_seq: held_seq,
                op: ReceiptOp::StreamOpen,
                op_hash,
                amount_micros: cap,
                from: Some(from.clone()),
                to: Some(to.clone()),
                balance_after_available_micros: holder_balance.available_micros,
                balance_after_held_micros: holder_balance.held_micros,
                ts,
            },
            signer,
        );

        Ok(StreamOpenOutcome {
            stream_id,
            first_seq: avail_seq,
            holder_balance,
            idempotent_replay: false,
        })
    }
}

/// The default deterministic signer used when a store is built without one. A
/// fixed seed + constant key id keeps receipt hashes AND signatures
/// byte-reproducible across runs (the AP2 golden/rebuild gates rely on this).
fn default_signer() -> Arc<dyn Signer> {
    Arc::new(FixedSigner::new())
}

/// In-memory, dependency-free ledger store for AP0/AP1/AP2.
pub struct InMemoryStore {
    state: Mutex<State>,
    clock: Arc<dyn Clock>,
    /// Signs each emitted receipt (injectable; deterministic in tests).
    signer: Arc<dyn Signer>,
    /// Test-only fault injection: when set, `commit_hold_capture` panics AFTER
    /// computing balances but BEFORE writing any row/projection, proving the
    /// single critical section is all-or-nothing (nothing half-applied).
    fail_before_capture_write: AtomicBool,
}

impl Default for InMemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

impl InMemoryStore {
    /// A store backed by the real system clock and the default deterministic
    /// signer.
    pub fn new() -> Self {
        Self::with_clock_and_signer(Arc::new(SystemClock), default_signer())
    }

    /// A store backed by an injected clock (deterministic in tests) and the
    /// default signer.
    pub fn with_clock(clock: Arc<dyn Clock>) -> Self {
        Self::with_clock_and_signer(clock, default_signer())
    }

    /// A store backed by the system clock and an injected signer.
    pub fn with_signer(signer: Arc<dyn Signer>) -> Self {
        Self::with_clock_and_signer(Arc::new(SystemClock), signer)
    }

    /// A store backed by both an injected clock and signer — the fully
    /// deterministic test constructor.
    pub fn with_clock_and_signer(clock: Arc<dyn Clock>, signer: Arc<dyn Signer>) -> Self {
        InMemoryStore {
            state: Mutex::new(State::default()),
            clock,
            signer,
            fail_before_capture_write: AtomicBool::new(false),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        // A poisoned lock means a prior panic inside the critical section.
        // Recover the guard, but fail-closed if the panic tore an in-flight
        // write (`writing == true`): serving half-applied state would break the
        // all-or-nothing guarantee (double-debit on retry, live != rebuild). A
        // panic BEFORE any write leaves `writing == false`, so the whole op was
        // dropped and the untouched state is safe to surface.
        let guard = self.state.lock().unwrap_or_else(|e| e.into_inner());
        assert!(
            !guard.writing,
            "ledger mutex poisoned mid-write: state is torn, refusing to serve"
        );
        guard
    }

    // ---- test/introspection helpers (concrete-type only) -------------------

    /// Snapshot of the full journal (append order). For tests/audits.
    pub fn journal_snapshot(&self) -> Vec<JournalEntry> {
        self.lock().journal.clone()
    }

    /// Snapshot of the live balance projection. For tests/audits.
    pub fn balances_snapshot(&self) -> HashMap<AccountId, Balance> {
        self.lock().balances.clone()
    }

    /// Snapshot of the live holds map. For tests/audits.
    pub fn holds_snapshot(&self) -> HashMap<HoldId, HoldRecord> {
        self.lock().holds.clone()
    }

    /// Snapshot of the live allowances map. For tests/audits.
    pub fn allowances_snapshot(&self) -> HashMap<AllowanceId, AllowanceRecord> {
        self.lock().allowances.clone()
    }

    /// Snapshot of the live streams map. For tests/audits.
    pub fn streams_snapshot(&self) -> HashMap<StreamId, StreamRecord> {
        self.lock().streams.clone()
    }

    /// The current in-store reserve total (µ$). For tests/audits.
    pub fn reserve_micros_snapshot(&self) -> Micros {
        self.lock().reserve_micros
    }

    /// Number of known accounts.
    pub fn accounts_len(&self) -> usize {
        self.lock().accounts.len()
    }

    /// Test-only: arm the capture fail-point so the next `commit_hold_capture`
    /// panics after computing balances but before any write.
    pub fn arm_capture_failpoint(&self) {
        self.fail_before_capture_write.store(true, Ordering::SeqCst);
    }

    /// Snapshot the full receipt log in global append order (for tests/audit and
    /// offline export).
    pub fn receipts_snapshot(&self) -> Vec<Receipt> {
        self.lock().receipts.global().to_vec()
    }

    /// Snapshot one account's receipt chain in append order.
    pub fn receipt_chain(&self, account: &AccountId) -> Vec<Receipt> {
        self.lock()
            .receipts
            .chain(account)
            .into_iter()
            .cloned()
            .collect()
    }

    /// The Merkle audit root over the whole live receipt log (§6 audit).
    pub fn merkle_root(&self) -> ReceiptHash {
        merkle_root(self.lock().receipts.global())
    }

    /// AP0-only opening-balance helper: append a single-sided seed row and
    /// credit the account so a fixed total supply exists.
    ///
    /// SECURITY: this is deliberately an inherent method on the concrete
    /// `InMemoryStore` and NOT part of the [`LedgerStore`] trait. Seeding adds
    /// `available` with NO backing `reserve_micros` leg, so it must never be
    /// reachable through the store trait that the AP4 `ReserveLedger` (or any
    /// production store) is generic over — otherwise it would mint unbacked AIT,
    /// bypass the deposit gate, and break `Σ(available + held) == reserve`. It
    /// remains available to AP0/test bootstrap code that holds the concrete type.
    pub async fn seed_account(&self, id: &AccountId, amount: Micros) -> Result<Balance, MintError> {
        let ts = self.clock.now();
        let mut st = self.lock();
        if !st.accounts.contains_key(id) {
            return Err(MintError::AccountNotFound);
        }
        let seq = st.next_seq;
        // Single-sided opening entry; reserve counterparty is off-book in AP0.
        let body = serde_json::json!({
            "op": "seed",
            "account": id.as_str(),
            "amount_micros": amount.get(),
            "seq": seq,
        });
        let op_hash = mint_core::op_hash(&body);
        let current = st
            .balances
            .get(id)
            .cloned()
            .unwrap_or_else(|| Balance::zeroed(id.clone()));
        let new_available = current.available_micros.checked_add(amount)?;

        // Enter the write phase: every op below is infallible/non-panicking, so a
        // panic can only strike before it (op dropped) — never mid-write.
        st.writing = true;
        st.journal.push(JournalEntry {
            seq,
            op: OpKind::Seed,
            op_hash,
            idem_key: format!("seed:{}:{}", id.as_str(), seq),
            account_id: id.clone(),
            counter_id: None,
            delta_micros: amount,
            bucket: Bucket::Available,
            ref_: None,
            ts,
        });
        st.next_seq = seq + 1;

        let updated = Balance {
            account_id: id.clone(),
            available_micros: new_available,
            // Preserve any existing held leg (an open hold must survive a seed).
            held_micros: current.held_micros,
            updated_seq: seq,
        };
        st.balances.insert(id.clone(), updated.clone());

        // Genesis receipt for this account's chain (op=Seed, prev_hash=ZERO).
        let draft = ReceiptDraft {
            account_id: id.clone(),
            journal_seq: seq,
            op: ReceiptOp::Seed,
            op_hash,
            amount_micros: amount,
            from: None,
            to: Some(id.clone()),
            balance_after_available_micros: updated.available_micros,
            balance_after_held_micros: updated.held_micros,
            ts,
        };
        st.receipts.append(draft, self.signer.as_ref());
        st.writing = false;

        Ok(updated)
    }
}

#[async_trait]
impl LedgerStore for InMemoryStore {
    async fn create_account(
        &self,
        kind: AccountKind,
        owner_ref: Option<String>,
    ) -> Result<AccountId, MintError> {
        let id = AccountId::new(kind, Ulid::new());
        let now = self.clock.now();
        let mut st = self.lock();
        st.accounts.insert(id.clone(), kind);
        st.account_meta.insert(id.clone(), (owner_ref, now));
        st.balances.insert(id.clone(), Balance::zeroed(id.clone()));
        Ok(id)
    }

    async fn create_address(
        &self,
        account_id: &AccountId,
        purpose: Option<String>,
    ) -> Result<Address, MintError> {
        let mut st = self.lock();
        if !st.accounts.contains_key(account_id) {
            return Err(MintError::AccountNotFound);
        }
        let purpose_str = purpose.unwrap_or_default();
        let data = seed_bytes16(&[account_id.as_str(), &purpose_str]);
        let addr = Address::encode(account_id.kind(), &data);
        st.addresses.insert(addr.clone(), account_id.clone());
        Ok(addr)
    }

    async fn lookup_address(&self, addr: &Address) -> Result<AccountId, MintError> {
        // Validate the checksum before touching the index.
        addr.decode()?;
        self.lock()
            .addresses
            .get(addr)
            .cloned()
            .ok_or(MintError::AccountNotFound)
    }

    async fn commit_transfer(&self, commit: TransferCommit) -> Result<CommitOutcome, MintError> {
        let ts = self.clock.now();
        let mut st = self.lock();

        // 1. Idempotency: replay returns the original outcome; a different body
        //    under the same key is a conflict. Nothing is written either way.
        if let Some(stored) = st.idem_index.get(&commit.idem_key) {
            if stored.op_hash != commit.op_hash {
                return Err(MintError::IdempotencyConflict);
            }
            match &stored.outcome {
                StoredOutcome::Transfer(out) => {
                    let mut out = out.clone();
                    out.idempotent_replay = true;
                    return Ok(out);
                }
                // Same key previously bound a different op kind: the hashes above
                // already matched, so this is unreachable in practice; treat a
                // cross-kind hash collision as a conflict.
                _ => return Err(MintError::IdempotencyConflict),
            }
        }

        // Account lifecycle guard, inside the critical section (a freeze cannot
        // interleave between this check and the write below).
        st.ensure_can_send_locked(&commit.from)?;
        st.ensure_can_receive_locked(&commit.to)?;

        // 2-5. Validate + append two rows + project + emit two receipts through
        //       the shared atomic double-entry path.
        let outcome = st.settle_transfer_locked(
            self.signer.as_ref(),
            &commit.from,
            &commit.to,
            commit.amount.micros(),
            commit.op_hash,
            &commit.idem_key,
            commit.ref_.clone(),
            ts,
        )?;

        // 6. Record the outcome under the idempotency key.
        st.idem_index.insert(
            commit.idem_key.clone(),
            StoredCommit {
                op_hash: commit.op_hash,
                outcome: StoredOutcome::Transfer(outcome.clone()),
            },
        );
        st.writing = false;
        Ok(outcome)
    }

    async fn commit_hold_create(&self, commit: HoldCreateCommit) -> Result<HoldOutcome, MintError> {
        let ts = self.clock.now();
        let mut st = self.lock();

        // 1. Idempotency: replay returns the ORIGINAL outcome (same hold_id);
        //    a different body under the same key is a conflict. Writes nothing.
        if let Some(stored) = st.idem_index.get(&commit.idem_key) {
            if stored.op_hash != commit.op_hash {
                return Err(MintError::IdempotencyConflict);
            }
            return match &stored.outcome {
                StoredOutcome::Hold(out) => {
                    let mut out = out.clone();
                    out.idempotent_replay = true;
                    Ok(out)
                }
                _ => Err(MintError::IdempotencyConflict),
            };
        }

        // Account lifecycle guard, inside the critical section (a freeze cannot
        // interleave between this check and the write below).
        st.ensure_can_send_locked(&commit.from)?;

        // 2-6. Reserve available→held, mint the hold, append both legs, open the
        //       record, emit one receipt — through the shared atomic path.
        let outcome = st.settle_hold_create_locked(
            self.signer.as_ref(),
            &commit.from,
            commit.amount.micros(),
            commit.op_hash,
            &commit.idem_key,
            commit.ref_.clone(),
            ts,
        )?;
        // The auto-release deadline is written in the same critical section.
        if let Some(expires_at) = commit.expires_at_ms {
            st.hold_expiries
                .entry(outcome.hold_id.clone())
                .or_insert(expires_at);
        }

        // 7. Record the outcome under the idempotency key.
        st.idem_index.insert(
            commit.idem_key.clone(),
            StoredCommit {
                op_hash: commit.op_hash,
                outcome: StoredOutcome::Hold(outcome.clone()),
            },
        );
        st.writing = false;
        Ok(outcome)
    }

    async fn commit_hold_capture(
        &self,
        commit: HoldCaptureCommit,
    ) -> Result<HoldOutcome, MintError> {
        let ts = self.clock.now();
        let mut st = self.lock();

        // 1. Idempotency (double-capture protection): identical replay returns
        //    the original outcome; payee is credited exactly once.
        if let Some(stored) = st.idem_index.get(&commit.idem_key) {
            if stored.op_hash != commit.op_hash {
                return Err(MintError::IdempotencyConflict);
            }
            return match &stored.outcome {
                StoredOutcome::Hold(out) => {
                    let mut out = out.clone();
                    out.idempotent_replay = true;
                    Ok(out)
                }
                _ => Err(MintError::IdempotencyConflict),
            };
        }

        // 2. Resolve the hold; a distinct close of a terminal hold is not-found.
        let hold = st
            .holds
            .get(&commit.hold_id)
            .cloned()
            .ok_or(MintError::HoldNotFound)?;
        if hold.status != HoldStatus::Open {
            return Err(MintError::HoldNotFound);
        }
        let holder = hold.account_id.clone();
        // Account lifecycle guard, inside the critical section (a freeze cannot
        // interleave between this check and the write below).
        st.ensure_can_send_locked(&holder)?;
        st.ensure_can_receive_locked(&commit.to)?;

        // 3. Capture must settle to a DIFFERENT account (self-alias guard).
        if commit.to == holder {
            return Err(MintError::BadAccountId);
        }

        // 4. capture <= hold.amount (the whole reservation).
        let capture = commit.amount.micros();
        let hold_amt = hold.amount_micros;
        if capture.get() > hold_amt.get() {
            return Err(MintError::CaptureExceedsHold);
        }
        let remainder = hold_amt.checked_sub(capture)?;

        // 5. Both balances must exist; checked math, no leg negative.
        let holder_bal = st
            .balances
            .get(&holder)
            .cloned()
            .ok_or(MintError::AccountNotFound)?;
        let to_bal = st
            .balances
            .get(&commit.to)
            .cloned()
            .ok_or(MintError::AccountNotFound)?;
        let new_holder_held = holder_bal.held_micros.checked_sub(hold_amt)?;
        if new_holder_held.is_negative() {
            return Err(MintError::InsufficientFunds);
        }
        let new_holder_available = holder_bal.available_micros.checked_add(remainder)?;
        let new_payee_available = to_bal.available_micros.checked_add(capture)?;
        let neg_hold_amt = hold_amt.checked_neg()?;

        // Test-only fault injection: crash AFTER computation, BEFORE any write.
        // Nothing below has mutated `st` yet, so the whole op is dropped.
        if self.fail_before_capture_write.swap(false, Ordering::SeqCst) {
            panic!("fault-injection: crash between capture computation and journal append");
        }

        // 6. Append rows: holder held leg (-hold_amt), payee available leg
        //    (+capture), and the remainder available leg on the holder if > 0.
        let hold_ref = Some(commit.hold_id.as_str().to_string());
        let held_row_seq = st.next_seq;
        let payee_row_seq = held_row_seq + 1;
        // Enter the write phase (after the fail-point above): every op below is
        // infallible/non-panicking, so a panic can only strike before it (op
        // dropped) — never mid-write.
        st.writing = true;
        st.journal.push(JournalEntry {
            seq: held_row_seq,
            op: OpKind::HoldCapture,
            op_hash: commit.op_hash,
            idem_key: commit.idem_key.clone(),
            account_id: holder.clone(),
            counter_id: Some(commit.to.clone()),
            delta_micros: neg_hold_amt,
            bucket: Bucket::Held,
            ref_: hold_ref.clone(),
            ts,
        });
        st.journal.push(JournalEntry {
            seq: payee_row_seq,
            op: OpKind::HoldCapture,
            op_hash: commit.op_hash,
            idem_key: commit.idem_key.clone(),
            account_id: commit.to.clone(),
            counter_id: Some(holder.clone()),
            delta_micros: capture,
            bucket: Bucket::Available,
            ref_: hold_ref.clone(),
            ts,
        });
        let mut next = payee_row_seq + 1;
        let mut holder_updated_seq = held_row_seq;
        if remainder.is_positive() {
            let rem_seq = next;
            st.journal.push(JournalEntry {
                seq: rem_seq,
                op: OpKind::HoldCapture,
                op_hash: commit.op_hash,
                idem_key: commit.idem_key.clone(),
                account_id: holder.clone(),
                counter_id: None,
                delta_micros: remainder,
                bucket: Bucket::Available,
                ref_: hold_ref,
                ts,
            });
            next = rem_seq + 1;
            holder_updated_seq = rem_seq;
        }
        st.next_seq = next;

        // 7. Projection.
        let holder_balance = Balance {
            account_id: holder.clone(),
            available_micros: new_holder_available,
            held_micros: new_holder_held,
            updated_seq: holder_updated_seq,
        };
        let payee_balance = Balance {
            account_id: commit.to.clone(),
            available_micros: new_payee_available,
            held_micros: to_bal.held_micros,
            updated_seq: payee_row_seq,
        };
        st.balances.insert(holder.clone(), holder_balance.clone());
        st.balances.insert(commit.to.clone(), payee_balance.clone());

        // 7b. Emit TWO receipts (holder then payee). The holder receipt carries
        //     the FINAL holder balance (incl. any remainder), so the optional
        //     remainder row does NOT add a third receipt.
        st.receipts.append(
            ReceiptDraft {
                account_id: holder.clone(),
                journal_seq: holder_updated_seq,
                op: ReceiptOp::HoldCapture,
                op_hash: commit.op_hash,
                amount_micros: capture,
                from: Some(holder.clone()),
                to: Some(commit.to.clone()),
                balance_after_available_micros: holder_balance.available_micros,
                balance_after_held_micros: holder_balance.held_micros,
                ts,
            },
            self.signer.as_ref(),
        );
        st.receipts.append(
            ReceiptDraft {
                account_id: commit.to.clone(),
                journal_seq: payee_row_seq,
                op: ReceiptOp::HoldCapture,
                op_hash: commit.op_hash,
                amount_micros: capture,
                from: Some(holder.clone()),
                to: Some(commit.to.clone()),
                balance_after_available_micros: payee_balance.available_micros,
                balance_after_held_micros: payee_balance.held_micros,
                ts,
            },
            self.signer.as_ref(),
        );

        // 8. Close the hold Captured.
        let record = HoldRecord {
            captured_micros: capture,
            status: HoldStatus::Captured,
            ..hold
        };
        st.holds.insert(commit.hold_id.clone(), record);

        // 9. Outcome + idem record.
        let outcome = HoldOutcome {
            hold_id: commit.hold_id.clone(),
            first_seq: held_row_seq,
            holder_balance,
            payee_balance: Some(payee_balance),
            status: HoldStatus::Captured,
            captured_micros: capture,
            idempotent_replay: false,
        };
        st.idem_index.insert(
            commit.idem_key.clone(),
            StoredCommit {
                op_hash: commit.op_hash,
                outcome: StoredOutcome::Hold(outcome.clone()),
            },
        );
        st.writing = false;
        Ok(outcome)
    }

    async fn commit_hold_release(
        &self,
        commit: HoldReleaseCommit,
    ) -> Result<HoldOutcome, MintError> {
        let ts = self.clock.now();
        let mut st = self.lock();

        // 1. Idempotency.
        if let Some(stored) = st.idem_index.get(&commit.idem_key) {
            if stored.op_hash != commit.op_hash {
                return Err(MintError::IdempotencyConflict);
            }
            return match &stored.outcome {
                StoredOutcome::Hold(out) => {
                    let mut out = out.clone();
                    out.idempotent_replay = true;
                    Ok(out)
                }
                _ => Err(MintError::IdempotencyConflict),
            };
        }

        // 2. Resolve the hold; must be Open (terminal distinct-close => not-found).
        let hold = st
            .holds
            .get(&commit.hold_id)
            .cloned()
            .ok_or(MintError::HoldNotFound)?;
        if hold.status != HoldStatus::Open {
            return Err(MintError::HoldNotFound);
        }
        let holder = hold.account_id.clone();

        // 3. Free the reservation: held -= amount, available += amount. No payee.
        let holder_bal = st
            .balances
            .get(&holder)
            .cloned()
            .ok_or(MintError::AccountNotFound)?;
        let hold_amt = hold.amount_micros;
        let new_held = holder_bal.held_micros.checked_sub(hold_amt)?;
        if new_held.is_negative() {
            return Err(MintError::InsufficientFunds);
        }
        let new_available = holder_bal.available_micros.checked_add(hold_amt)?;
        let neg_hold_amt = hold_amt.checked_neg()?;

        // 4. Append two rows on the holder (held leg then available leg).
        let hold_ref = Some(commit.hold_id.as_str().to_string());
        let held_seq = st.next_seq;
        let avail_seq = held_seq + 1;
        // Enter the write phase: every op below is infallible/non-panicking, so a
        // panic can only strike before it (op dropped) — never mid-write.
        st.writing = true;
        st.journal.push(JournalEntry {
            seq: held_seq,
            op: OpKind::HoldRelease,
            op_hash: commit.op_hash,
            idem_key: commit.idem_key.clone(),
            account_id: holder.clone(),
            counter_id: None,
            delta_micros: neg_hold_amt,
            bucket: Bucket::Held,
            ref_: hold_ref.clone(),
            ts,
        });
        st.journal.push(JournalEntry {
            seq: avail_seq,
            op: OpKind::HoldRelease,
            op_hash: commit.op_hash,
            idem_key: commit.idem_key.clone(),
            account_id: holder.clone(),
            counter_id: None,
            delta_micros: hold_amt,
            bucket: Bucket::Available,
            ref_: hold_ref,
            ts,
        });
        st.next_seq = avail_seq + 1;

        // 5. Projection.
        let holder_balance = Balance {
            account_id: holder.clone(),
            available_micros: new_available,
            held_micros: new_held,
            updated_seq: avail_seq,
        };
        st.balances.insert(holder.clone(), holder_balance.clone());

        // 5b. Emit ONE receipt on the holder (no payee; funds returned to avail).
        st.receipts.append(
            ReceiptDraft {
                account_id: holder.clone(),
                journal_seq: avail_seq,
                op: ReceiptOp::HoldRelease,
                op_hash: commit.op_hash,
                amount_micros: hold_amt,
                from: Some(holder.clone()),
                to: None,
                balance_after_available_micros: holder_balance.available_micros,
                balance_after_held_micros: holder_balance.held_micros,
                ts,
            },
            self.signer.as_ref(),
        );

        // 6. Close the hold Released (captured stays 0).
        let record = HoldRecord {
            status: HoldStatus::Released,
            ..hold
        };
        st.holds.insert(commit.hold_id.clone(), record);

        // 7. Outcome + idem record.
        let outcome = HoldOutcome {
            hold_id: commit.hold_id.clone(),
            first_seq: held_seq,
            holder_balance,
            payee_balance: None,
            status: HoldStatus::Released,
            captured_micros: Micros::zero(),
            idempotent_replay: false,
        };
        st.idem_index.insert(
            commit.idem_key.clone(),
            StoredCommit {
                op_hash: commit.op_hash,
                outcome: StoredOutcome::Hold(outcome.clone()),
            },
        );
        st.writing = false;
        Ok(outcome)
    }

    async fn commit_allowance_grant(
        &self,
        commit: AllowanceGrantCommit,
    ) -> Result<AllowanceGrantOutcome, MintError> {
        let now = self.clock.now();
        let mut st = self.lock();

        // 1. Idempotency: replay returns the ORIGINAL id; a different body under
        //    the same key is a conflict. Nothing is created either way.
        if let Some(stored) = st.idem_index.get(&commit.idem_key) {
            if stored.op_hash != commit.op_hash {
                return Err(MintError::IdempotencyConflict);
            }
            return match &stored.outcome {
                StoredOutcome::AllowanceGrant(out) => {
                    let mut out = out.clone();
                    out.idempotent_replay = true;
                    Ok(out)
                }
                _ => Err(MintError::IdempotencyConflict),
            };
        }

        // Account lifecycle guard, inside the critical section (a freeze cannot
        // interleave between this check and the write below).
        st.ensure_can_send_locked(&commit.granter)?;

        // 2. Granter must exist (a grant delegates the granter's funds).
        if !st.accounts.contains_key(&commit.granter) {
            return Err(MintError::AccountNotFound);
        }

        // 3. Mint the id and open the record. A grant moves NO money and appends
        //    NO journal/receipt rows (a control-plane op, like create_account);
        //    the money only moves on `commit_allowance_spend`.
        let id = AllowanceId::new(Ulid::new());
        let record = AllowanceRecord {
            id: id.clone(),
            granter: commit.granter.clone(),
            to_key: commit.to_key.clone(),
            limit_micros: commit.limit.micros(),
            spent_micros: Micros::zero(),
            window: commit.window,
            window_start_ms: now,
            scope: commit.scope.clone(),
            revoked: false,
        };
        st.allowances.insert(id.clone(), record);

        let outcome = AllowanceGrantOutcome {
            allowance_id: id,
            idempotent_replay: false,
        };
        st.idem_index.insert(
            commit.idem_key.clone(),
            StoredCommit {
                op_hash: commit.op_hash,
                outcome: StoredOutcome::AllowanceGrant(outcome.clone()),
            },
        );
        Ok(outcome)
    }

    async fn commit_allowance_spend(
        &self,
        commit: AllowanceSpendCommit,
    ) -> Result<AllowanceSpendOutcome, MintError> {
        let ts = self.clock.now();
        let mut st = self.lock();

        // 1. Idempotency: an identical retry replays the ORIGINAL outcome; the
        //    money moved (and `spent` bumped) exactly once.
        if let Some(stored) = st.idem_index.get(&commit.idem_key) {
            if stored.op_hash != commit.op_hash {
                return Err(MintError::IdempotencyConflict);
            }
            return match &stored.outcome {
                StoredOutcome::AllowanceSpend(out) => {
                    let mut out = out.clone();
                    out.idempotent_replay = true;
                    Ok(out)
                }
                _ => Err(MintError::IdempotencyConflict),
            };
        }

        // 2. Load the allowance record (a working clone we roll + bump).
        let mut record = st
            .allowances
            .get(&commit.allowance_id)
            .cloned()
            .ok_or(MintError::AllowanceNotFound)?;
        // Account lifecycle guard, inside the critical section (a freeze cannot
        // interleave between this check and the write below).
        st.ensure_can_send_locked(&record.granter)?;
        st.ensure_can_receive_locked(&commit.to)?;

        // 3. A revoked allowance can never spend again.
        if record.revoked {
            return Err(MintError::AllowanceExceeded);
        }

        // 4. Roll the window: a rolling budget resets once its window elapsed.
        if let AllowanceWindow::Rolling { window_ms } = record.window {
            if ts.saturating_sub(record.window_start_ms) >= window_ms {
                record.spent_micros = Micros::zero();
                record.window_start_ms = ts;
            }
        }

        // 5. Scope: an unrestricted field (None) always matches; a set field must
        //    match the spend's payee / ref exactly (prefix for the resource).
        if let Some(scope) = &record.scope {
            if let Some(payee) = &scope.payee {
                if payee != commit.to.as_str() {
                    return Err(MintError::AllowanceExceeded);
                }
            }
            if let Some(prefix) = &scope.resource_prefix {
                let matches = commit
                    .ref_
                    .as_deref()
                    .map(|r| r.starts_with(prefix.as_str()))
                    .unwrap_or(false);
                if !matches {
                    return Err(MintError::AllowanceExceeded);
                }
            }
        }

        // 6. Budget check (checked add guards overflow): spent + amount <= limit.
        let amount = commit.amount.micros();
        let new_spent = record.spent_micros.checked_add(amount)?;
        if new_spent.get() > record.limit_micros.get() {
            return Err(MintError::AllowanceExceeded);
        }

        // 7. Auto-settle through the SAME atomic journal/receipt path a plain
        //    transfer/hold uses, so conservation + chain integrity hold. The
        //    limit check (6) and this money move share one critical section, so
        //    concurrent spends can never both pass the check and overspend.
        let (settlement_seq, hold_id) = match commit.scheme {
            AllowanceScheme::Transfer => {
                let out = st.settle_transfer_locked(
                    self.signer.as_ref(),
                    &record.granter,
                    &commit.to,
                    amount,
                    commit.op_hash,
                    &commit.idem_key,
                    commit.ref_.clone(),
                    ts,
                )?;
                (out.first_seq, None)
            }
            AllowanceScheme::Hold => {
                let out = st.settle_hold_create_locked(
                    self.signer.as_ref(),
                    &record.granter,
                    amount,
                    commit.op_hash,
                    &commit.idem_key,
                    commit.ref_.clone(),
                    ts,
                )?;
                (out.first_seq, Some(out.hold_id))
            }
        };

        // 8. Bump `spent` ATOMICALLY with the money move (same lock), and persist
        //    any window roll from step 4.
        record.spent_micros = new_spent;
        let remaining = record.limit_micros.checked_sub(new_spent)?;
        st.allowances.insert(commit.allowance_id.clone(), record);

        // 9. Outcome + idempotency record.
        let outcome = AllowanceSpendOutcome {
            allowance_id: commit.allowance_id.clone(),
            remaining_micros: remaining,
            settlement_seq,
            hold_id,
            idempotent_replay: false,
        };
        st.idem_index.insert(
            commit.idem_key.clone(),
            StoredCommit {
                op_hash: commit.op_hash,
                outcome: StoredOutcome::AllowanceSpend(outcome.clone()),
            },
        );
        st.writing = false;
        Ok(outcome)
    }

    async fn commit_allowance_revoke(
        &self,
        commit: AllowanceRevokeCommit,
    ) -> Result<AllowanceId, MintError> {
        let mut st = self.lock();
        let mut record = st
            .allowances
            .get(&commit.allowance_id)
            .cloned()
            .ok_or(MintError::AllowanceNotFound)?;
        // Setting `revoked` is naturally idempotent — a second revoke is a no-op.
        record.revoked = true;
        st.allowances.insert(commit.allowance_id.clone(), record);
        Ok(commit.allowance_id)
    }

    async fn get_allowance(&self, id: &AllowanceId) -> Result<AllowanceRecord, MintError> {
        let record = self
            .lock()
            .allowances
            .get(id)
            .cloned()
            .ok_or(MintError::AllowanceNotFound)?;
        // See the matching comment in mint-store-postgres::Store::get_allowance:
        // project the `Rolling` window's lazy reset onto this read instead of
        // returning a stale pre-reset `spent_micros`. Read-only.
        Ok(record.as_of(self.clock.now()))
    }

    async fn commit_stream_open(
        &self,
        commit: StreamOpenCommit,
    ) -> Result<StreamOpenOutcome, MintError> {
        let ts = self.clock.now();
        let mut st = self.lock();

        // 1. Idempotency: replay returns the ORIGINAL outcome (same stream_id).
        if let Some(stored) = st.idem_index.get(&commit.idem_key) {
            if stored.op_hash != commit.op_hash {
                return Err(MintError::IdempotencyConflict);
            }
            return match &stored.outcome {
                StoredOutcome::StreamOpen(out) => {
                    let mut out = out.clone();
                    out.idempotent_replay = true;
                    Ok(out)
                }
                _ => Err(MintError::IdempotencyConflict),
            };
        }

        // Account lifecycle guard, inside the critical section (a freeze cannot
        // interleave between this check and the write below).
        st.ensure_can_send_locked(&commit.from)?;
        st.ensure_can_receive_locked(&commit.to)?;

        // 2. Reserve the whole cap available→held, mint the stream, open record.
        let outcome = st.settle_stream_open_locked(
            self.signer.as_ref(),
            &commit.from,
            &commit.to,
            commit.rate.micros(),
            commit.cap.micros(),
            commit.unit,
            commit.op_hash,
            &commit.idem_key,
            commit.ref_.clone(),
            ts,
        )?;

        st.idem_index.insert(
            commit.idem_key.clone(),
            StoredCommit {
                op_hash: commit.op_hash,
                outcome: StoredOutcome::StreamOpen(outcome.clone()),
            },
        );
        st.writing = false;
        Ok(outcome)
    }

    async fn commit_stream_tick(
        &self,
        commit: StreamTickCommit,
    ) -> Result<StreamTickOutcome, MintError> {
        let ts = self.clock.now();
        let mut st = self.lock();

        // 1. Idempotency: a retried tick replays the ORIGINAL outcome exactly once.
        if let Some(stored) = st.idem_index.get(&commit.idem_key) {
            if stored.op_hash != commit.op_hash {
                return Err(MintError::IdempotencyConflict);
            }
            return match &stored.outcome {
                StoredOutcome::StreamTick(out) => {
                    let mut out = out.clone();
                    out.idempotent_replay = true;
                    Ok(out)
                }
                _ => Err(MintError::IdempotencyConflict),
            };
        }

        // 2. Resolve the stream; must be Open.
        let stream = st
            .streams
            .get(&commit.stream_id)
            .cloned()
            .ok_or(MintError::StreamNotFound)?;
        if stream.status != StreamStatus::Open {
            return Err(MintError::StreamClosed);
        }
        // Account lifecycle guard, inside the critical section (a freeze cannot
        // interleave between this check and the write below).
        st.ensure_can_send_locked(&stream.from_id)?;
        st.ensure_can_receive_locked(&stream.to_id)?;

        // 3. Monotonic seq (strictly increasing).
        if commit.units == 0 {
            return Err(MintError::InvalidAmount);
        }
        if commit.seq <= stream.last_seq {
            return Err(MintError::StreamSeqRegressed);
        }

        // 4. amount = units * rate; must not exceed remaining cap.
        let amount = tick_amount(commit.units, stream.rate_micros)?;
        let new_settled = stream.settled_micros.checked_add(amount)?;
        if new_settled.get() > stream.cap_micros.get() {
            return Err(MintError::StreamCapExceeded);
        }

        // 5. Move held(from) → available(to). No partial application on failure.
        let from = stream.from_id.clone();
        let to = stream.to_id.clone();
        let from_bal = st
            .balances
            .get(&from)
            .cloned()
            .ok_or(MintError::AccountNotFound)?;
        let to_bal = st
            .balances
            .get(&to)
            .cloned()
            .ok_or(MintError::AccountNotFound)?;
        let new_from_held = from_bal.held_micros.checked_sub(amount)?;
        if new_from_held.is_negative() {
            return Err(MintError::InsufficientFunds);
        }
        let new_to_available = to_bal.available_micros.checked_add(amount)?;
        let neg_amount = amount.checked_neg()?;

        // 6. Append two rows sharing this tick's unique op_hash.
        let stream_ref = Some(commit.stream_id.as_str().to_string());
        let held_seq = st.next_seq;
        let avail_seq = held_seq + 1;
        st.writing = true;
        st.journal.push(JournalEntry {
            seq: held_seq,
            op: OpKind::StreamTick,
            op_hash: commit.op_hash,
            idem_key: commit.idem_key.clone(),
            account_id: from.clone(),
            counter_id: Some(to.clone()),
            delta_micros: neg_amount,
            bucket: Bucket::Held,
            ref_: stream_ref.clone(),
            ts,
        });
        st.journal.push(JournalEntry {
            seq: avail_seq,
            op: OpKind::StreamTick,
            op_hash: commit.op_hash,
            idem_key: commit.idem_key.clone(),
            account_id: to.clone(),
            counter_id: Some(from.clone()),
            delta_micros: amount,
            bucket: Bucket::Available,
            ref_: stream_ref,
            ts,
        });
        st.next_seq = avail_seq + 1;

        // 7. Projection.
        let from_balance = Balance {
            account_id: from.clone(),
            available_micros: from_bal.available_micros,
            held_micros: new_from_held,
            updated_seq: held_seq,
        };
        let to_balance = Balance {
            account_id: to.clone(),
            available_micros: new_to_available,
            held_micros: to_bal.held_micros,
            updated_seq: avail_seq,
        };
        st.balances.insert(from.clone(), from_balance.clone());
        st.balances.insert(to.clone(), to_balance.clone());

        // 7b. Two receipts: payer (held reduced) then payee (available raised).
        st.receipts.append(
            ReceiptDraft {
                account_id: from.clone(),
                journal_seq: held_seq,
                op: ReceiptOp::StreamTick,
                op_hash: commit.op_hash,
                amount_micros: amount,
                from: Some(from.clone()),
                to: Some(to.clone()),
                balance_after_available_micros: from_balance.available_micros,
                balance_after_held_micros: from_balance.held_micros,
                ts,
            },
            self.signer.as_ref(),
        );
        st.receipts.append(
            ReceiptDraft {
                account_id: to.clone(),
                journal_seq: avail_seq,
                op: ReceiptOp::StreamTick,
                op_hash: commit.op_hash,
                amount_micros: amount,
                from: Some(from.clone()),
                to: Some(to.clone()),
                balance_after_available_micros: to_balance.available_micros,
                balance_after_held_micros: to_balance.held_micros,
                ts,
            },
            self.signer.as_ref(),
        );

        // 8. Bump the stream (settled + last_seq), status stays Open.
        let record = StreamRecord {
            settled_micros: new_settled,
            last_seq: commit.seq,
            ..stream
        };
        st.streams.insert(commit.stream_id.clone(), record);

        let outcome = StreamTickOutcome {
            stream_id: commit.stream_id.clone(),
            tick_seq: commit.seq,
            settled_total_micros: new_settled,
            from_balance,
            to_balance,
            idempotent_replay: false,
        };
        st.idem_index.insert(
            commit.idem_key.clone(),
            StoredCommit {
                op_hash: commit.op_hash,
                outcome: StoredOutcome::StreamTick(outcome.clone()),
            },
        );
        st.writing = false;
        Ok(outcome)
    }

    async fn commit_stream_close(
        &self,
        commit: StreamCloseCommit,
    ) -> Result<StreamCloseOutcome, MintError> {
        let ts = self.clock.now();
        let mut st = self.lock();

        // 1. Idempotency.
        if let Some(stored) = st.idem_index.get(&commit.idem_key) {
            if stored.op_hash != commit.op_hash {
                return Err(MintError::IdempotencyConflict);
            }
            return match &stored.outcome {
                StoredOutcome::StreamClose(out) => {
                    let mut out = out.clone();
                    out.idempotent_replay = true;
                    Ok(out)
                }
                _ => Err(MintError::IdempotencyConflict),
            };
        }

        // 2. Resolve the stream; must be Open.
        let stream = st
            .streams
            .get(&commit.stream_id)
            .cloned()
            .ok_or(MintError::StreamNotFound)?;
        if stream.status != StreamStatus::Open {
            return Err(MintError::StreamClosed);
        }
        let from = stream.from_id.clone();
        let to = stream.to_id.clone();
        // A zero-unit close only releases the payer's own remainder (allowed on
        // a frozen account); a final tick is a payment and is guarded.
        if commit.units > 0 {
            st.ensure_can_send_locked(&from)?;
            st.ensure_can_receive_locked(&to)?;
        }

        // 3. Optional final tick (same guards as a tick when units > 0).
        let amount = if commit.units > 0 {
            if commit.seq <= stream.last_seq {
                return Err(MintError::StreamSeqRegressed);
            }
            tick_amount(commit.units, stream.rate_micros)?
        } else {
            Micros::zero()
        };
        let new_settled = stream.settled_micros.checked_add(amount)?;
        if new_settled.get() > stream.cap_micros.get() {
            return Err(MintError::StreamCapExceeded);
        }
        let remainder = stream.cap_micros.checked_sub(new_settled)?;
        let held_cleared = amount.checked_add(remainder)?; // == held before close

        // 4. Compute balances (checked; held → 0 on the payer).
        let from_bal = st
            .balances
            .get(&from)
            .cloned()
            .ok_or(MintError::AccountNotFound)?;
        let to_bal = st
            .balances
            .get(&to)
            .cloned()
            .ok_or(MintError::AccountNotFound)?;
        let new_from_held = from_bal.held_micros.checked_sub(held_cleared)?;
        if new_from_held.is_negative() {
            return Err(MintError::InsufficientFunds);
        }
        let new_from_available = from_bal.available_micros.checked_add(remainder)?;
        let new_to_available = to_bal.available_micros.checked_add(amount)?;

        // 5. Build the legs (fixed order): final settle (payee), then release
        //    (payer), then a zero-delta marker if neither applies — so every
        //    close writes ≥1 row and the Open→Closed transition is
        //    journal-derivable. All share the close op_hash.
        let mut legs: Vec<(AccountId, Micros, Bucket)> = Vec::new();
        if amount.is_positive() {
            legs.push((from.clone(), amount.checked_neg()?, Bucket::Held));
            legs.push((to.clone(), amount, Bucket::Available));
        }
        if remainder.is_positive() {
            legs.push((from.clone(), remainder.checked_neg()?, Bucket::Held));
            legs.push((from.clone(), remainder, Bucket::Available));
        }
        if legs.is_empty() {
            // Degenerate close (cap already fully settled): a zero-delta marker
            // row records the terminal transition for the rebuild projection.
            legs.push((from.clone(), Micros::zero(), Bucket::Held));
        }

        let stream_ref = Some(commit.stream_id.as_str().to_string());
        let first_close_seq = st.next_seq;
        st.writing = true;
        let mut payee_row_seq: Option<u64> = None;
        let mut seq = first_close_seq;
        for (account, delta, bucket) in legs {
            // The payee settle leg is the Available leg credited to `to`.
            if bucket == Bucket::Available && account == to {
                payee_row_seq = Some(seq);
            }
            st.journal.push(JournalEntry {
                seq,
                op: OpKind::StreamClose,
                op_hash: commit.op_hash,
                idem_key: commit.idem_key.clone(),
                account_id: account,
                counter_id: Some(to.clone()),
                delta_micros: delta,
                bucket,
                ref_: stream_ref.clone(),
                ts,
            });
            seq += 1;
        }
        st.next_seq = seq;

        // 6. Projection.
        let from_balance = Balance {
            account_id: from.clone(),
            available_micros: new_from_available,
            held_micros: new_from_held,
            updated_seq: seq - 1,
        };
        st.balances.insert(from.clone(), from_balance.clone());
        let to_balance = if amount.is_positive() {
            let b = Balance {
                account_id: to.clone(),
                available_micros: new_to_available,
                held_micros: to_bal.held_micros,
                updated_seq: payee_row_seq.expect("payee row seq set when amount > 0"),
            };
            st.balances.insert(to.clone(), b.clone());
            b
        } else {
            to_bal
        };

        // 7. Receipts: payee (if a final tick settled) then payer (final balance).
        if amount.is_positive() {
            st.receipts.append(
                ReceiptDraft {
                    account_id: to.clone(),
                    journal_seq: payee_row_seq.expect("payee row seq set when amount > 0"),
                    op: ReceiptOp::StreamClose,
                    op_hash: commit.op_hash,
                    amount_micros: amount,
                    from: Some(from.clone()),
                    to: Some(to.clone()),
                    balance_after_available_micros: to_balance.available_micros,
                    balance_after_held_micros: to_balance.held_micros,
                    ts,
                },
                self.signer.as_ref(),
            );
        }
        st.receipts.append(
            ReceiptDraft {
                account_id: from.clone(),
                journal_seq: first_close_seq,
                op: ReceiptOp::StreamClose,
                op_hash: commit.op_hash,
                amount_micros: held_cleared,
                from: Some(from.clone()),
                to: Some(to.clone()),
                balance_after_available_micros: from_balance.available_micros,
                balance_after_held_micros: from_balance.held_micros,
                ts,
            },
            self.signer.as_ref(),
        );

        // 8. Close the stream (terminal).
        let record = StreamRecord {
            settled_micros: new_settled,
            last_seq: if commit.units > 0 {
                commit.seq
            } else {
                stream.last_seq
            },
            status: StreamStatus::Closed,
            ..stream
        };
        st.streams.insert(commit.stream_id.clone(), record);

        let outcome = StreamCloseOutcome {
            stream_id: commit.stream_id.clone(),
            settled_total_micros: new_settled,
            released_micros: remainder,
            from_balance,
            to_balance,
            idempotent_replay: false,
        };
        st.idem_index.insert(
            commit.idem_key.clone(),
            StoredCommit {
                op_hash: commit.op_hash,
                outcome: StoredOutcome::StreamClose(outcome.clone()),
            },
        );
        st.writing = false;
        Ok(outcome)
    }

    async fn get_stream(&self, id: &StreamId) -> Result<StreamRecord, MintError> {
        self.lock()
            .streams
            .get(id)
            .cloned()
            .ok_or(MintError::StreamNotFound)
    }

    async fn rebuild_streams(&self) -> Result<HashMap<StreamId, StreamRecord>, MintError> {
        let st = self.lock();
        let mut streams: HashMap<StreamId, StreamRecord> = HashMap::new();
        for entry in &st.journal {
            let stream_id = match (entry.op, entry.ref_.as_deref()) {
                (OpKind::StreamOpen | OpKind::StreamTick | OpKind::StreamClose, Some(r)) => {
                    StreamId::parse(r)?
                }
                _ => continue,
            };
            match entry.op {
                // The Held leg (+cap) of open seeds the reservation record.
                OpKind::StreamOpen if entry.bucket == Bucket::Held => {
                    let to = entry.counter_id.clone().ok_or(MintError::AccountNotFound)?;
                    streams.entry(stream_id.clone()).or_insert(StreamRecord {
                        id: stream_id.clone(),
                        from_id: entry.account_id.clone(),
                        to_id: to,
                        // rate/unit/ref are not journal-derivable; the projection
                        // reconstructs lifecycle (status/settled/cap/last_seq).
                        rate_micros: Micros::zero(),
                        unit: StreamUnit::Second,
                        cap_micros: entry.delta_micros,
                        settled_micros: Micros::zero(),
                        last_seq: 0,
                        status: StreamStatus::Open,
                        ref_: None,
                    });
                }
                // The payee Available leg (account == payee) carries the settled
                // amount; the payer Held leg is skipped here.
                OpKind::StreamTick if entry.bucket == Bucket::Available => {
                    if let Some(rec) = streams.get_mut(&stream_id) {
                        if entry.account_id == rec.to_id {
                            rec.settled_micros =
                                rec.settled_micros.checked_add(entry.delta_micros)?;
                        }
                    }
                }
                // A close's payee Available leg adds any final-tick settlement.
                OpKind::StreamClose if entry.bucket == Bucket::Available => {
                    if let Some(rec) = streams.get_mut(&stream_id) {
                        if entry.account_id == rec.to_id {
                            rec.settled_micros =
                                rec.settled_micros.checked_add(entry.delta_micros)?;
                        }
                        rec.status = StreamStatus::Closed;
                    }
                }
                // A close with no settlement/release still marks the terminal
                // transition via its (marker or release-held) row.
                OpKind::StreamClose if entry.bucket == Bucket::Held => {
                    if let Some(rec) = streams.get_mut(&stream_id) {
                        rec.status = StreamStatus::Closed;
                    }
                }
                _ => {}
            }
        }
        Ok(streams)
    }

    async fn commit_mint(&self, commit: MintCommit) -> Result<MintOutcome, MintError> {
        let ts = self.clock.now();
        let mut st = self.lock();

        // 1. Idempotency: a confirmed deposit mints EXACTLY once.
        if let Some(stored) = st.idem_index.get(&commit.idem_key) {
            if stored.op_hash != commit.op_hash {
                return Err(MintError::IdempotencyConflict);
            }
            return match &stored.outcome {
                StoredOutcome::Mint(out) => {
                    let mut out = out.clone();
                    out.idempotent_replay = true;
                    Ok(out)
                }
                _ => Err(MintError::IdempotencyConflict),
            };
        }

        // Account lifecycle guard, inside the critical section (a freeze cannot
        // interleave between this check and the write below).
        st.ensure_can_receive_locked(&commit.to)?;

        // 2. The credited account must exist.
        let to_bal = st
            .balances
            .get(&commit.to)
            .cloned()
            .ok_or(MintError::AccountNotFound)?;
        let amount = commit.amount.micros();
        // Both LHS (available) and RHS (reserve) rise by the same amount, so the
        // conservation equality Σ(avail+held) == reserve is preserved.
        let new_available = to_bal.available_micros.checked_add(amount)?;
        let new_reserve = st.reserve_micros.checked_add(amount)?;

        let seq = st.next_seq;
        st.writing = true;
        st.journal.push(JournalEntry {
            seq,
            op: OpKind::Mint,
            op_hash: commit.op_hash,
            idem_key: commit.idem_key.clone(),
            account_id: commit.to.clone(),
            counter_id: None,
            delta_micros: amount,
            bucket: Bucket::Available,
            ref_: Some(commit.deposit_ref.clone()),
            ts,
        });
        st.next_seq = seq + 1;
        st.reserve_micros = new_reserve;

        let to_balance = Balance {
            account_id: commit.to.clone(),
            available_micros: new_available,
            held_micros: to_bal.held_micros,
            updated_seq: seq,
        };
        st.balances.insert(commit.to.clone(), to_balance.clone());

        st.receipts.append(
            ReceiptDraft {
                account_id: commit.to.clone(),
                journal_seq: seq,
                op: ReceiptOp::Mint,
                op_hash: commit.op_hash,
                amount_micros: amount,
                from: None,
                to: Some(commit.to.clone()),
                balance_after_available_micros: to_balance.available_micros,
                balance_after_held_micros: to_balance.held_micros,
                ts,
            },
            self.signer.as_ref(),
        );

        let outcome = MintOutcome {
            first_seq: seq,
            to_balance,
            reserve_after_micros: new_reserve,
            idempotent_replay: false,
        };
        st.idem_index.insert(
            commit.idem_key.clone(),
            StoredCommit {
                op_hash: commit.op_hash,
                outcome: StoredOutcome::Mint(outcome.clone()),
            },
        );
        st.writing = false;
        Ok(outcome)
    }

    async fn commit_redeem(&self, commit: RedeemCommit) -> Result<RedeemOutcome, MintError> {
        let ts = self.clock.now();
        let mut st = self.lock();

        // 1. Idempotency: a payout burns EXACTLY once.
        if let Some(stored) = st.idem_index.get(&commit.idem_key) {
            if stored.op_hash != commit.op_hash {
                return Err(MintError::IdempotencyConflict);
            }
            return match &stored.outcome {
                StoredOutcome::Redeem(out) => {
                    let mut out = out.clone();
                    out.idempotent_replay = true;
                    Ok(out)
                }
                _ => Err(MintError::IdempotencyConflict),
            };
        }

        // Account lifecycle guard, inside the critical section. An operator
        // claw-back burn (refund / chargeback) bypasses it by design.
        if !commit.bypass_lifecycle {
            st.ensure_can_send_locked(&commit.from)?;
        }

        // 2. The debited account must exist and hold enough available.
        let from_bal = st
            .balances
            .get(&commit.from)
            .cloned()
            .ok_or(MintError::AccountNotFound)?;
        let amount = commit.amount.micros();
        let new_available = from_bal.available_micros.checked_sub(amount)?;
        if new_available.is_negative() {
            return Err(MintError::InsufficientFunds);
        }
        // Defense-in-depth: the reserve must cover the burn. Both LHS and RHS
        // fall by the same amount, preserving conservation.
        if st.reserve_micros.get() < amount.get() {
            return Err(MintError::ReserveInsufficient);
        }
        let new_reserve = st.reserve_micros.checked_sub(amount)?;
        let neg_amount = amount.checked_neg()?;

        let seq = st.next_seq;
        st.writing = true;
        st.journal.push(JournalEntry {
            seq,
            op: OpKind::Redeem,
            op_hash: commit.op_hash,
            idem_key: commit.idem_key.clone(),
            account_id: commit.from.clone(),
            counter_id: None,
            delta_micros: neg_amount,
            bucket: Bucket::Available,
            ref_: Some(commit.payout_ref.clone()),
            ts,
        });
        st.next_seq = seq + 1;
        st.reserve_micros = new_reserve;

        let from_balance = Balance {
            account_id: commit.from.clone(),
            available_micros: new_available,
            held_micros: from_bal.held_micros,
            updated_seq: seq,
        };
        st.balances
            .insert(commit.from.clone(), from_balance.clone());

        st.receipts.append(
            ReceiptDraft {
                account_id: commit.from.clone(),
                journal_seq: seq,
                op: ReceiptOp::Redeem,
                op_hash: commit.op_hash,
                amount_micros: amount,
                from: Some(commit.from.clone()),
                to: None,
                balance_after_available_micros: from_balance.available_micros,
                balance_after_held_micros: from_balance.held_micros,
                ts,
            },
            self.signer.as_ref(),
        );

        let outcome = RedeemOutcome {
            first_seq: seq,
            from_balance,
            reserve_after_micros: new_reserve,
            idempotent_replay: false,
        };
        st.idem_index.insert(
            commit.idem_key.clone(),
            StoredCommit {
                op_hash: commit.op_hash,
                outcome: StoredOutcome::Redeem(outcome.clone()),
            },
        );
        st.writing = false;
        Ok(outcome)
    }

    async fn reserve_micros(&self) -> Micros {
        self.lock().reserve_micros
    }

    async fn get_balance(&self, account_id: &AccountId) -> Result<Balance, MintError> {
        self.lock()
            .balances
            .get(account_id)
            .cloned()
            .ok_or(MintError::AccountNotFound)
    }

    async fn rebuild_balances(&self) -> Result<HashMap<AccountId, Balance>, MintError> {
        let st = self.lock();
        // Start every known account at zero so accounts with no journal rows
        // match the live projection (updated_seq == 0) bit-for-bit.
        let mut rebuilt: HashMap<AccountId, Balance> = st
            .accounts
            .keys()
            .map(|id| (id.clone(), Balance::zeroed(id.clone())))
            .collect();

        for entry in &st.journal {
            let bal = rebuilt
                .entry(entry.account_id.clone())
                .or_insert_with(|| Balance::zeroed(entry.account_id.clone()));
            // Route each signed delta to the leg named on the row so held is
            // reconstructed bit-exact alongside available (AP1 gate).
            match entry.bucket {
                Bucket::Available => {
                    bal.available_micros = bal.available_micros.checked_add(entry.delta_micros)?;
                }
                Bucket::Held => {
                    bal.held_micros = bal.held_micros.checked_add(entry.delta_micros)?;
                }
            }
            bal.updated_seq = entry.seq;
        }
        Ok(rebuilt)
    }

    async fn rebuild_holds(&self) -> Result<HashMap<HoldId, HoldRecord>, MintError> {
        let st = self.lock();
        let mut holds: HashMap<HoldId, HoldRecord> = HashMap::new();
        for entry in &st.journal {
            // Every hold row carries hold_id in ref_.
            let hold_id = match (entry.op, entry.ref_.as_deref()) {
                (OpKind::HoldCreate | OpKind::HoldCapture | OpKind::HoldRelease, Some(r)) => {
                    HoldId::parse(r)?
                }
                _ => continue,
            };
            match entry.op {
                // The Held leg (+amount) of create seeds the reservation.
                OpKind::HoldCreate if entry.bucket == Bucket::Held => {
                    holds.entry(hold_id.clone()).or_insert(HoldRecord {
                        id: hold_id.clone(),
                        account_id: entry.account_id.clone(),
                        amount_micros: entry.delta_micros,
                        captured_micros: Micros::zero(),
                        status: HoldStatus::Open,
                        ref_: None,
                    });
                }
                // The payee Available leg (account != holder) carries the
                // captured amount; other capture legs are the holder's.
                OpKind::HoldCapture if entry.bucket == Bucket::Available => {
                    if let Some(rec) = holds.get_mut(&hold_id) {
                        if entry.account_id != rec.account_id {
                            rec.captured_micros = entry.delta_micros;
                            rec.status = HoldStatus::Captured;
                        }
                    }
                }
                OpKind::HoldRelease if entry.bucket == Bucket::Held => {
                    if let Some(rec) = holds.get_mut(&hold_id) {
                        rec.status = HoldStatus::Released;
                    }
                }
                _ => {}
            }
        }
        Ok(holds)
    }

    async fn journal_len(&self) -> u64 {
        self.lock().journal.len() as u64
    }

    async fn rebuild_receipts(&self) -> Result<ReceiptLog, MintError> {
        let st = self.lock();
        let signer = self.signer.as_ref();
        let mut log = ReceiptLog::new();

        // Running per-account balance legs, folded exactly like rebuild_balances
        // so each receipt's balance_after matches the live projection.
        let mut avail: HashMap<AccountId, Micros> = HashMap::new();
        let mut held: HashMap<AccountId, Micros> = HashMap::new();
        let read = |avail: &HashMap<AccountId, Micros>,
                    held: &HashMap<AccountId, Micros>,
                    acct: &AccountId| {
            (
                avail.get(acct).copied().unwrap_or_else(Micros::zero),
                held.get(acct).copied().unwrap_or_else(Micros::zero),
            )
        };

        let journal = &st.journal;
        let mut i = 0;
        while i < journal.len() {
            // One op's rows are contiguous and share a content-derived op_hash,
            // which is unique per committed op (the canonical op body folds in the
            // idem_key). idem_key is NOT a safe delimiter here: seed_account
            // bypasses the idem_index with a synthetic `seed:<acct>:<seq>` key, so
            // a user-chosen transfer/hold idem_key that equals a preceding seed's
            // key would merge two distinct ops into one group and drop receipts.
            let op_hash = journal[i].op_hash;
            let mut j = i;
            while j < journal.len() && journal[j].op_hash == op_hash {
                j += 1;
            }
            let group = &journal[i..j];

            // A legitimate group is homogeneous (one op kind). A mixed group means
            // a bad delimiter merged distinct ops; fail loudly instead of silently
            // dropping/merging receipts (op_hash equality is already guaranteed by
            // the grouping loop above).
            if group.iter().any(|r| r.op != group[0].op) {
                return Err(MintError::ReceiptRebuildMismatch);
            }

            // Apply every leg of the op before reading post-op balances.
            for row in group {
                match row.bucket {
                    Bucket::Available => {
                        let e = avail
                            .entry(row.account_id.clone())
                            .or_insert_with(Micros::zero);
                        *e = e.checked_add(row.delta_micros)?;
                    }
                    Bucket::Held => {
                        let e = held
                            .entry(row.account_id.clone())
                            .or_insert_with(Micros::zero);
                        *e = e.checked_add(row.delta_micros)?;
                    }
                }
            }

            let ts = group[0].ts;
            let op_hash = group[0].op_hash;
            match group[0].op {
                OpKind::Seed => {
                    // 1 row: available += amount, single-sided.
                    let acct = group[0].account_id.clone();
                    let (a, h) = read(&avail, &held, &acct);
                    log.append(
                        ReceiptDraft {
                            account_id: acct.clone(),
                            journal_seq: group[0].seq,
                            op: ReceiptOp::Seed,
                            op_hash,
                            amount_micros: group[0].delta_micros,
                            from: None,
                            to: Some(acct),
                            balance_after_available_micros: a,
                            balance_after_held_micros: h,
                            ts,
                        },
                        signer,
                    );
                }
                OpKind::Transfer => {
                    // 2 rows: [debit(from), credit(to)]. Emit from then to.
                    let from = group[0].account_id.clone();
                    let to = group[1].account_id.clone();
                    let amount = group[1].delta_micros; // +amount (credit leg)
                    let (fa, fh) = read(&avail, &held, &from);
                    log.append(
                        ReceiptDraft {
                            account_id: from.clone(),
                            journal_seq: group[0].seq,
                            op: ReceiptOp::Transfer,
                            op_hash,
                            amount_micros: amount,
                            from: Some(from.clone()),
                            to: Some(to.clone()),
                            balance_after_available_micros: fa,
                            balance_after_held_micros: fh,
                            ts,
                        },
                        signer,
                    );
                    let (ta, th) = read(&avail, &held, &to);
                    log.append(
                        ReceiptDraft {
                            account_id: to.clone(),
                            journal_seq: group[1].seq,
                            op: ReceiptOp::Transfer,
                            op_hash,
                            amount_micros: amount,
                            from: Some(from),
                            to: Some(to),
                            balance_after_available_micros: ta,
                            balance_after_held_micros: th,
                            ts,
                        },
                        signer,
                    );
                }
                OpKind::HoldCreate => {
                    // 2 rows: [available(-), held(+)] on the holder.
                    let holder = group[0].account_id.clone();
                    let amount = group[1].delta_micros; // +amount (held leg)
                    let (a, h) = read(&avail, &held, &holder);
                    log.append(
                        ReceiptDraft {
                            account_id: holder.clone(),
                            journal_seq: group[1].seq,
                            op: ReceiptOp::HoldCreate,
                            op_hash,
                            amount_micros: amount,
                            from: Some(holder),
                            to: None,
                            balance_after_available_micros: a,
                            balance_after_held_micros: h,
                            ts,
                        },
                        signer,
                    );
                }
                OpKind::HoldCapture => {
                    // 2 or 3 rows: [held(holder,-), payee_avail(to,+capture),
                    // (remainder holder,+)?]. Emit holder then payee.
                    let holder = group[0].account_id.clone();
                    let payee = group[1].account_id.clone();
                    let capture = group[1].delta_micros;
                    let holder_seq = if group.len() == 3 {
                        group[2].seq
                    } else {
                        group[0].seq
                    };
                    let (ha, hh) = read(&avail, &held, &holder);
                    log.append(
                        ReceiptDraft {
                            account_id: holder.clone(),
                            journal_seq: holder_seq,
                            op: ReceiptOp::HoldCapture,
                            op_hash,
                            amount_micros: capture,
                            from: Some(holder.clone()),
                            to: Some(payee.clone()),
                            balance_after_available_micros: ha,
                            balance_after_held_micros: hh,
                            ts,
                        },
                        signer,
                    );
                    let (pa, ph) = read(&avail, &held, &payee);
                    log.append(
                        ReceiptDraft {
                            account_id: payee.clone(),
                            journal_seq: group[1].seq,
                            op: ReceiptOp::HoldCapture,
                            op_hash,
                            amount_micros: capture,
                            from: Some(holder),
                            to: Some(payee),
                            balance_after_available_micros: pa,
                            balance_after_held_micros: ph,
                            ts,
                        },
                        signer,
                    );
                }
                OpKind::HoldRelease => {
                    // 2 rows: [held(-), available(+)] on the holder.
                    let holder = group[0].account_id.clone();
                    let amount = group[1].delta_micros; // +hold_amt (available leg)
                    let (a, h) = read(&avail, &held, &holder);
                    log.append(
                        ReceiptDraft {
                            account_id: holder.clone(),
                            journal_seq: group[1].seq,
                            op: ReceiptOp::HoldRelease,
                            op_hash,
                            amount_micros: amount,
                            from: Some(holder),
                            to: None,
                            balance_after_available_micros: a,
                            balance_after_held_micros: h,
                            ts,
                        },
                        signer,
                    );
                }
                OpKind::StreamOpen => {
                    // 2 rows: [available(-cap), held(+cap)] on the payer.
                    let holder = group[0].account_id.clone();
                    let to = group[0].counter_id.clone();
                    let cap = group[1].delta_micros; // +cap (held leg)
                    let (a, h) = read(&avail, &held, &holder);
                    log.append(
                        ReceiptDraft {
                            account_id: holder.clone(),
                            journal_seq: group[1].seq,
                            op: ReceiptOp::StreamOpen,
                            op_hash,
                            amount_micros: cap,
                            from: Some(holder),
                            to,
                            balance_after_available_micros: a,
                            balance_after_held_micros: h,
                            ts,
                        },
                        signer,
                    );
                }
                OpKind::StreamTick => {
                    // 2 rows: [held(payer,-amount), avail(payee,+amount)].
                    let from = group[0].account_id.clone();
                    let to = group[1].account_id.clone();
                    let amount = group[1].delta_micros; // +amount (payee leg)
                    let (fa, fh) = read(&avail, &held, &from);
                    log.append(
                        ReceiptDraft {
                            account_id: from.clone(),
                            journal_seq: group[0].seq,
                            op: ReceiptOp::StreamTick,
                            op_hash,
                            amount_micros: amount,
                            from: Some(from.clone()),
                            to: Some(to.clone()),
                            balance_after_available_micros: fa,
                            balance_after_held_micros: fh,
                            ts,
                        },
                        signer,
                    );
                    let (ta, th) = read(&avail, &held, &to);
                    log.append(
                        ReceiptDraft {
                            account_id: to.clone(),
                            journal_seq: group[1].seq,
                            op: ReceiptOp::StreamTick,
                            op_hash,
                            amount_micros: amount,
                            from: Some(from),
                            to: Some(to),
                            balance_after_available_micros: ta,
                            balance_after_held_micros: th,
                            ts,
                        },
                        signer,
                    );
                }
                OpKind::StreamClose => {
                    // Variable rows: optional [held(payer,-amt), avail(payee,+amt)]
                    // settle, optional [held(payer,-rem), avail(payer,+rem)]
                    // release, or a lone zero-delta marker. Emit payee (if a final
                    // tick settled) then payer, mirroring the live path.
                    let holder = group[0].account_id.clone();
                    let to = group[0].counter_id.clone();
                    // The payee settle leg is an Available leg whose account is the
                    // stream payee (≠ the payer holder).
                    let payee_row = group.iter().find(|r| {
                        r.bucket == Bucket::Available && Some(&r.account_id) == to.as_ref()
                    });
                    // Total held cleared from the payer (settle + release).
                    let mut held_cleared = Micros::zero();
                    for r in group {
                        if r.bucket == Bucket::Held && r.account_id == holder {
                            held_cleared = held_cleared.checked_sub(r.delta_micros)?;
                        }
                    }
                    if let Some(pr) = payee_row {
                        let payee = pr.account_id.clone();
                        let (pa, ph) = read(&avail, &held, &payee);
                        log.append(
                            ReceiptDraft {
                                account_id: payee.clone(),
                                journal_seq: pr.seq,
                                op: ReceiptOp::StreamClose,
                                op_hash,
                                amount_micros: pr.delta_micros,
                                from: Some(holder.clone()),
                                to: Some(payee),
                                balance_after_available_micros: pa,
                                balance_after_held_micros: ph,
                                ts,
                            },
                            signer,
                        );
                    }
                    let (ha, hh) = read(&avail, &held, &holder);
                    log.append(
                        ReceiptDraft {
                            account_id: holder.clone(),
                            journal_seq: group[0].seq,
                            op: ReceiptOp::StreamClose,
                            op_hash,
                            amount_micros: held_cleared,
                            from: Some(holder),
                            to,
                            balance_after_available_micros: ha,
                            balance_after_held_micros: hh,
                            ts,
                        },
                        signer,
                    );
                }
                OpKind::Mint => {
                    // 1 row: available += amount, single-sided; reserve rose too.
                    let acct = group[0].account_id.clone();
                    let (a, h) = read(&avail, &held, &acct);
                    log.append(
                        ReceiptDraft {
                            account_id: acct.clone(),
                            journal_seq: group[0].seq,
                            op: ReceiptOp::Mint,
                            op_hash,
                            amount_micros: group[0].delta_micros,
                            from: None,
                            to: Some(acct),
                            balance_after_available_micros: a,
                            balance_after_held_micros: h,
                            ts,
                        },
                        signer,
                    );
                }
                OpKind::Redeem => {
                    // 1 row: available -= amount, single-sided; reserve fell too.
                    let acct = group[0].account_id.clone();
                    let amount = group[0].delta_micros.checked_neg()?; // burn is +amount
                    let (a, h) = read(&avail, &held, &acct);
                    log.append(
                        ReceiptDraft {
                            account_id: acct.clone(),
                            journal_seq: group[0].seq,
                            op: ReceiptOp::Redeem,
                            op_hash,
                            amount_micros: amount,
                            from: Some(acct),
                            to: None,
                            balance_after_available_micros: a,
                            balance_after_held_micros: h,
                            ts,
                        },
                        signer,
                    );
                }
            }
            i = j;
        }
        Ok(log)
    }

    async fn snapshot_receipts(&self) -> Vec<Receipt> {
        self.receipts_snapshot()
    }

    async fn get_receipt(&self, account_id: &AccountId, journal_seq: u64) -> Option<Receipt> {
        self.receipts_snapshot().into_iter().find(|receipt| {
            &receipt.body.account_id == account_id && receipt.body.journal_seq == journal_seq
        })
    }

    async fn receipt_merkle_root(&self) -> ReceiptHash {
        self.merkle_root()
    }

    async fn list_accounts(
        &self,
        after: Option<AccountId>,
        limit: usize,
    ) -> Result<Vec<AccountSummary>, MintError> {
        let st = self.lock();
        let mut ids: Vec<&AccountId> = st
            .accounts
            .keys()
            .filter(|id| after.as_ref().is_none_or(|a| id.as_str() > a.as_str()))
            .collect();
        ids.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        Ok(ids
            .into_iter()
            .take(limit)
            .map(|id| {
                let meta = st.account_meta.get(id);
                AccountSummary {
                    balance: st
                        .balances
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| Balance::zeroed(id.clone())),
                    owner_ref: meta.and_then(|m| m.0.clone()),
                    created_ms: meta.map(|m| m.1),
                    status: st.account_status.get(id).copied().unwrap_or_default(),
                }
            })
            .collect())
    }

    async fn upsert_payout(&self, record: PayoutRecord) -> Result<PayoutRecord, MintError> {
        let now = self.clock.now();
        let mut st = self.lock();
        let created_ms = st
            .payouts
            .get(&record.payout_ref)
            .map(|existing| existing.created_ms)
            .unwrap_or(now);
        let stored = PayoutRecord {
            created_ms,
            updated_ms: now,
            ..record
        };
        st.payouts
            .insert(stored.payout_ref.clone(), stored.clone());
        Ok(stored)
    }

    async fn get_payout(&self, payout_ref: &str) -> Result<Option<PayoutRecord>, MintError> {
        Ok(self.lock().payouts.get(payout_ref).cloned())
    }

    async fn list_payouts(&self, q: PayoutQuery) -> Result<Vec<PayoutRecord>, MintError> {
        let st = self.lock();
        let mut rows: Vec<PayoutRecord> = st
            .payouts
            .values()
            .filter(|p| q.status.is_none_or(|s| p.status == s))
            .filter(|p| q.account_id.as_ref().is_none_or(|a| &p.account_id == a))
            .filter(|p| {
                q.before.as_ref().is_none_or(|(ms, r)| {
                    (p.created_ms, p.payout_ref.as_str()) < (*ms, r.as_str())
                })
            })
            .cloned()
            .collect();
        rows.sort_by(|a, b| {
            (b.created_ms, b.payout_ref.as_str()).cmp(&(a.created_ms, a.payout_ref.as_str()))
        });
        rows.truncate(q.limit);
        Ok(rows)
    }

    async fn reserve_movement_totals(&self) -> Result<Vec<ReserveMovementTotal>, MintError> {
        let st = self.lock();
        let mut acc: HashMap<(String, String), (u64, i64)> = HashMap::new();
        for e in &st.journal {
            let kind = match e.op {
                OpKind::Mint => "mint",
                OpKind::Redeem => "redeem",
                _ => continue,
            };
            let tag = provider_tag_of(e.ref_.as_deref().unwrap_or(""));
            let slot = acc.entry((kind.to_string(), tag)).or_insert((0, 0));
            slot.0 += 1;
            slot.1 = slot.1.saturating_add(e.delta_micros.get().saturating_abs());
        }
        let mut out: Vec<ReserveMovementTotal> = acc
            .into_iter()
            .map(|((kind, provider), (count, total))| ReserveMovementTotal {
                kind,
                provider,
                count,
                total_micros: total,
            })
            .collect();
        out.sort_by(|a, b| (&a.kind, &a.provider).cmp(&(&b.kind, &b.provider)));
        Ok(out)
    }

    async fn list_reserve_movements(
        &self,
        provider: &str,
        from_ms: Option<i64>,
        to_ms: Option<i64>,
        limit: usize,
    ) -> Result<Vec<ReserveMovement>, MintError> {
        let st = self.lock();
        Ok(st
            .journal
            .iter()
            .filter_map(|e| {
                let kind = match e.op {
                    OpKind::Mint => "mint",
                    OpKind::Redeem => "redeem",
                    _ => return None,
                };
                let reference = e.ref_.clone().unwrap_or_default();
                if provider_tag_of(&reference) != provider {
                    return None;
                }
                if from_ms.is_some_and(|f| e.ts < f) || to_ms.is_some_and(|t| e.ts > t) {
                    return None;
                }
                Some(ReserveMovement {
                    kind: kind.to_string(),
                    reference,
                    account_id: Some(e.account_id.as_str().to_string()),
                    amount_micros: e.delta_micros.get().saturating_abs(),
                    ts_ms: e.ts,
                })
            })
            .take(limit)
            .collect())
    }

    async fn get_account(&self, account_id: &AccountId) -> Result<AccountSummary, MintError> {
        let st = self.lock();
        Self::summary_locked(&st, account_id)
    }

    async fn set_account_status(
        &self,
        account_id: &AccountId,
        status: AccountStatus,
    ) -> Result<AccountSummary, MintError> {
        let mut st = self.lock();
        if !st.accounts.contains_key(account_id) {
            return Err(MintError::AccountNotFound);
        }
        let current = st.account_status.get(account_id).copied().unwrap_or_default();
        if current == AccountStatus::Closed && status != AccountStatus::Closed {
            return Err(MintError::AccountClosed);
        }
        if status == AccountStatus::Closed && current != AccountStatus::Closed {
            let bal = st
                .balances
                .get(account_id)
                .cloned()
                .unwrap_or_else(|| Balance::zeroed(account_id.clone()));
            if bal.available_micros.get() != 0 || bal.held_micros.get() != 0 {
                return Err(MintError::AccountNotEmpty);
            }
        }
        st.account_status.insert(account_id.clone(), status);
        Self::summary_locked(&st, account_id)
    }

    async fn get_hold(&self, id: &HoldId) -> Result<HoldRecord, MintError> {
        self.lock()
            .holds
            .get(id)
            .cloned()
            .ok_or(MintError::HoldNotFound)
    }

    async fn insert_api_key(&self, record: ApiKeyRecord) -> Result<ApiKeyRecord, MintError> {
        let mut st = self.lock();
        if st.api_keys.iter().any(|k| k.id == record.id || k.key_hash == record.key_hash) {
            return Err(MintError::IdempotencyConflict);
        }
        st.api_keys.push(record.clone());
        Ok(record)
    }

    async fn list_api_keys(&self) -> Result<Vec<ApiKeyRecord>, MintError> {
        let mut keys = self.lock().api_keys.clone();
        keys.sort_by(|a, b| (b.created_ms, &b.id).cmp(&(a.created_ms, &a.id)));
        Ok(keys)
    }

    async fn find_api_key(&self, key_hash: &str) -> Result<Option<ApiKeyRecord>, MintError> {
        Ok(self
            .lock()
            .api_keys
            .iter()
            .find(|k| k.key_hash == key_hash && k.revoked_ms.is_none())
            .cloned())
    }

    async fn list_account_holds(
        &self,
        account_id: &AccountId,
        status: Option<HoldStatus>,
        limit: usize,
    ) -> Result<Vec<(HoldRecord, Option<i64>)>, MintError> {
        let st = self.lock();
        if !st.accounts.contains_key(account_id) {
            return Err(MintError::AccountNotFound);
        }
        let mut out: Vec<(HoldRecord, Option<i64>)> = st
            .holds
            .values()
            .filter(|h| &h.account_id == account_id)
            .filter(|h| status.map_or(true, |s| h.status == s))
            .map(|h| (h.clone(), st.hold_expiries.get(&h.id).copied()))
            .collect();
        // Ids are ULIDs: descending id == newest first.
        out.sort_by(|a, b| b.0.id.as_str().cmp(a.0.id.as_str()));
        out.truncate(limit);
        Ok(out)
    }

    async fn list_account_streams(
        &self,
        account_id: &AccountId,
        status: Option<StreamStatus>,
        limit: usize,
    ) -> Result<Vec<StreamRecord>, MintError> {
        let st = self.lock();
        if !st.accounts.contains_key(account_id) {
            return Err(MintError::AccountNotFound);
        }
        let mut out: Vec<StreamRecord> = st
            .streams
            .values()
            .filter(|s| &s.from_id == account_id || &s.to_id == account_id)
            .filter(|s| status.map_or(true, |want| s.status == want))
            .cloned()
            .collect();
        out.sort_by(|a, b| b.id.as_str().cmp(a.id.as_str()));
        out.truncate(limit);
        Ok(out)
    }

    async fn list_account_allowances(
        &self,
        account_id: &AccountId,
        include_revoked: bool,
        limit: usize,
    ) -> Result<Vec<AllowanceRecord>, MintError> {
        let st = self.lock();
        if !st.accounts.contains_key(account_id) {
            return Err(MintError::AccountNotFound);
        }
        let mut out: Vec<AllowanceRecord> = st
            .allowances
            .values()
            .filter(|a| &a.granter == account_id)
            .filter(|a| include_revoked || !a.revoked)
            .cloned()
            .collect();
        out.sort_by(|a, b| b.id.as_str().cmp(a.id.as_str()));
        out.truncate(limit);
        Ok(out)
    }

    async fn set_hold_expiry(&self, hold_id: &HoldId, expires_at_ms: i64) -> Result<(), MintError> {
        let mut st = self.lock();
        if !st.holds.contains_key(hold_id) {
            return Err(MintError::HoldNotFound);
        }
        st.hold_expiries.insert(hold_id.clone(), expires_at_ms);
        Ok(())
    }

    async fn hold_expiry(&self, hold_id: &HoldId) -> Result<Option<i64>, MintError> {
        Ok(self.lock().hold_expiries.get(hold_id).copied())
    }

    async fn list_expired_holds(&self, now_ms: i64, limit: usize) -> Result<Vec<HoldId>, MintError> {
        let st = self.lock();
        let mut due: Vec<(i64, HoldId)> = st
            .hold_expiries
            .iter()
            .filter(|(_, &at)| at <= now_ms)
            .filter(|(id, _)| {
                st.holds
                    .get(*id)
                    .map_or(false, |h| h.status == HoldStatus::Open)
            })
            .map(|(id, &at)| (at, id.clone()))
            .collect();
        due.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.as_str().cmp(b.1.as_str())));
        Ok(due.into_iter().take(limit).map(|(_, id)| id).collect())
    }

    async fn insert_payer_key(&self, record: PayerKeyRecord) -> Result<PayerKeyRecord, MintError> {
        let mut st = self.lock();
        let clash = st.payer_keys.iter().any(|k| {
            k.payer == record.payer && k.pubkey_id == record.pubkey_id && k.revoked_ms.is_none()
        });
        if clash {
            return Err(MintError::IdempotencyConflict);
        }
        st.payer_keys.push(record.clone());
        Ok(record)
    }

    async fn list_payer_keys(&self, payer: Option<&str>) -> Result<Vec<PayerKeyRecord>, MintError> {
        let st = self.lock();
        let mut out: Vec<PayerKeyRecord> = st
            .payer_keys
            .iter()
            .filter(|k| payer.map_or(true, |p| k.payer == p))
            .cloned()
            .collect();
        out.sort_by(|a, b| b.created_ms.cmp(&a.created_ms));
        Ok(out)
    }

    async fn revoke_payer_key(
        &self,
        payer: &str,
        pubkey_id: &str,
    ) -> Result<Option<PayerKeyRecord>, MintError> {
        let now = self.clock.now();
        let mut st = self.lock();
        if let Some(k) = st
            .payer_keys
            .iter_mut()
            .find(|k| k.payer == payer && k.pubkey_id == pubkey_id && k.revoked_ms.is_none())
        {
            k.revoked_ms = Some(now);
            return Ok(Some(k.clone()));
        }
        Ok(st
            .payer_keys
            .iter()
            .rev()
            .find(|k| k.payer == payer && k.pubkey_id == pubkey_id)
            .cloned())
    }

    async fn revoke_api_key(&self, id: &str) -> Result<Option<ApiKeyRecord>, MintError> {
        let now = self.clock.now();
        let mut st = self.lock();
        let Some(k) = st.api_keys.iter_mut().find(|k| k.id == id) else {
            return Ok(None);
        };
        if k.revoked_ms.is_none() {
            k.revoked_ms = Some(now);
        }
        Ok(Some(k.clone()))
    }

    async fn insert_provider_report(
        &self,
        report: ProviderReport,
        lines: Vec<ProviderReportLine>,
    ) -> Result<ProviderReport, MintError> {
        let mut st = self.lock();
        if st.provider_reports.iter().any(|(r, _)| r.id == report.id) {
            return Err(MintError::IdempotencyConflict);
        }
        st.provider_reports.push((report.clone(), lines));
        Ok(report)
    }

    async fn list_provider_reports(&self, limit: usize) -> Result<Vec<ProviderReport>, MintError> {
        let st = self.lock();
        let mut out: Vec<ProviderReport> =
            st.provider_reports.iter().map(|(r, _)| r.clone()).collect();
        out.sort_by(|a, b| (b.imported_ms, &b.id).cmp(&(a.imported_ms, &a.id)));
        out.truncate(limit);
        Ok(out)
    }

    async fn get_provider_report(
        &self,
        id: &str,
    ) -> Result<Option<(ProviderReport, Vec<ProviderReportLine>)>, MintError> {
        Ok(self
            .lock()
            .provider_reports
            .iter()
            .find(|(r, _)| r.id == id)
            .cloned())
    }

    async fn delete_provider_report(&self, id: &str) -> Result<bool, MintError> {
        let mut st = self.lock();
        let before = st.provider_reports.len();
        st.provider_reports.retain(|(r, _)| r.id != id);
        Ok(st.provider_reports.len() != before)
    }

    async fn upsert_deposit_reversal(
        &self,
        record: DepositReversal,
    ) -> Result<DepositReversal, MintError> {
        let now = self.clock.now();
        let mut st = self.lock();
        let created_ms = st
            .reversals
            .get(&record.deposit_ref)
            .map(|r| r.created_ms)
            .unwrap_or(now);
        let stored = DepositReversal {
            created_ms,
            updated_ms: now,
            ..record
        };
        st.reversals
            .insert(stored.deposit_ref.clone(), stored.clone());
        Ok(stored)
    }

    async fn get_deposit_reversal(
        &self,
        deposit_ref: &str,
    ) -> Result<Option<DepositReversal>, MintError> {
        Ok(self.lock().reversals.get(deposit_ref).cloned())
    }

    async fn list_deposit_reversals(
        &self,
        status: Option<ReversalStatus>,
        limit: usize,
    ) -> Result<Vec<DepositReversal>, MintError> {
        let st = self.lock();
        let mut out: Vec<DepositReversal> = st
            .reversals
            .values()
            .filter(|r| status.is_none_or(|s| r.status == s))
            .cloned()
            .collect();
        out.sort_by(|a, b| (b.created_ms, &b.deposit_ref).cmp(&(a.created_ms, &a.deposit_ref)));
        out.truncate(limit);
        Ok(out)
    }
}

impl InMemoryStore {
    /// One account's summary, read under the held lock.
    fn summary_locked(st: &State, account_id: &AccountId) -> Result<AccountSummary, MintError> {
        if !st.accounts.contains_key(account_id) {
            return Err(MintError::AccountNotFound);
        }
        let meta = st.account_meta.get(account_id);
        Ok(AccountSummary {
            balance: st
                .balances
                .get(account_id)
                .cloned()
                .unwrap_or_else(|| Balance::zeroed(account_id.clone())),
            owner_ref: meta.and_then(|m| m.0.clone()),
            created_ms: meta.map(|m| m.1),
            status: st.account_status.get(account_id).copied().unwrap_or_default(),
        })
    }
}

/// `units * rate` in microdollars, checked. A tick/close settlement never
/// silently wraps: an out-of-range product is [`MintError::Overflow`].
fn tick_amount(units: u64, rate: Micros) -> Result<Micros, MintError> {
    let u = i64::try_from(units).map_err(|_| MintError::Overflow)?;
    u.checked_mul(rate.get())
        .map(Micros::new)
        .ok_or(MintError::Overflow)
}

/// Convenience: whole AIT → microdollars (checked). Handy for seeding.
pub fn ait_to_micros(ait: i64) -> Result<Micros, MintError> {
    ait.checked_mul(MICROS_PER_AIT)
        .map(Micros::new)
        .ok_or(MintError::Overflow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;

    fn store() -> InMemoryStore {
        InMemoryStore::with_clock(Arc::new(FixedClock::new(1_700_000_000_000)))
    }

    async fn account(s: &InMemoryStore, kind: AccountKind, seed: i64) -> AccountId {
        let id = s.create_account(kind, None).await.unwrap();
        if seed != 0 {
            s.seed_account(&id, Micros::new(seed)).await.unwrap();
        }
        id
    }

    fn commit(from: &AccountId, to: &AccountId, amount: i64, key: &str) -> TransferCommit {
        TransferCommit {
            from: from.clone(),
            to: to.clone(),
            amount: mint_core::TransferAmount::new(Micros::new(amount)).unwrap(),
            idem_key: key.to_string(),
            op_hash: mint_core::op_hash(&serde_json::json!({
                "from": from.as_str(), "to": to.as_str(), "amount": amount, "key": key,
            })),
            ref_: None,
        }
    }

    #[tokio::test]
    async fn transfer_appends_two_rows_and_moves_balance() {
        let s = store();
        let a = account(&s, AccountKind::User, 10_000_000).await;
        let b = account(&s, AccountKind::User, 0).await;
        // seed produced 1 journal row.
        assert_eq!(s.journal_len().await, 1);

        let out = s
            .commit_transfer(commit(&a, &b, 4_000_000, "k1"))
            .await
            .unwrap();
        assert!(!out.idempotent_replay);
        assert_eq!(s.journal_len().await, 3); // 1 seed + 2 transfer rows
        assert_eq!(out.from_balance.available_micros, Micros::new(6_000_000));
        assert_eq!(out.to_balance.available_micros, Micros::new(4_000_000));
    }

    #[tokio::test]
    async fn insufficient_funds_writes_nothing() {
        let s = store();
        let a = account(&s, AccountKind::User, 1_000_000).await;
        let b = account(&s, AccountKind::User, 0).await;
        let len_before = s.journal_len().await;
        let bal_before = s.get_balance(&a).await.unwrap();

        let err = s
            .commit_transfer(commit(&a, &b, 5_000_000, "k1"))
            .await
            .unwrap_err();
        assert_eq!(err, MintError::InsufficientFunds);
        assert_eq!(s.journal_len().await, len_before); // all-or-nothing
        assert_eq!(s.get_balance(&a).await.unwrap(), bal_before);
        assert_eq!(
            s.get_balance(&b).await.unwrap().available_micros,
            Micros::zero()
        );
    }

    #[tokio::test]
    async fn idempotent_replay_is_a_no_op() {
        let s = store();
        let a = account(&s, AccountKind::User, 10_000_000).await;
        let b = account(&s, AccountKind::User, 0).await;
        let first = s
            .commit_transfer(commit(&a, &b, 3_000_000, "k1"))
            .await
            .unwrap();
        let len = s.journal_len().await;

        for _ in 0..5 {
            let replay = s
                .commit_transfer(commit(&a, &b, 3_000_000, "k1"))
                .await
                .unwrap();
            assert!(replay.idempotent_replay);
            assert_eq!(replay.first_seq, first.first_seq);
            assert_eq!(replay.from_balance, first.from_balance);
            assert_eq!(s.journal_len().await, len);
        }
    }

    #[tokio::test]
    async fn same_key_different_body_conflicts() {
        let s = store();
        let a = account(&s, AccountKind::User, 10_000_000).await;
        let b = account(&s, AccountKind::User, 0).await;
        s.commit_transfer(commit(&a, &b, 3_000_000, "k1"))
            .await
            .unwrap();
        let len = s.journal_len().await;
        let err = s
            .commit_transfer(commit(&a, &b, 9_000_000, "k1"))
            .await
            .unwrap_err();
        assert_eq!(err, MintError::IdempotencyConflict);
        assert_eq!(s.journal_len().await, len);
    }

    #[tokio::test]
    async fn rebuild_equals_live_projection() {
        let s = store();
        let a = account(&s, AccountKind::User, 10_000_000).await;
        let b = account(&s, AccountKind::User, 5_000_000).await;
        let c = account(&s, AccountKind::User, 0).await; // never touched
        s.commit_transfer(commit(&a, &b, 3_000_000, "k1"))
            .await
            .unwrap();
        s.commit_transfer(commit(&b, &c, 1_000_000, "k2"))
            .await
            .unwrap();

        let rebuilt = s.rebuild_balances().await.unwrap();
        let live = s.balances_snapshot();
        assert_eq!(rebuilt, live);
        // spot-check the untouched account stays at updated_seq 0.
        assert_eq!(rebuilt[&c].available_micros, Micros::new(1_000_000));
    }

    #[tokio::test]
    async fn missing_account_is_reported() {
        let s = store();
        let a = account(&s, AccountKind::User, 10_000_000).await;
        let ghost = AccountId::new(AccountKind::User, Ulid::new());
        let err = s
            .commit_transfer(commit(&a, &ghost, 1, "k1"))
            .await
            .unwrap_err();
        assert_eq!(err, MintError::AccountNotFound);
        assert_eq!(s.journal_len().await, 1); // only the seed row
    }

    #[tokio::test]
    async fn address_create_and_lookup() {
        let s = store();
        let a = account(&s, AccountKind::Model, 0).await;
        let addr = s.create_address(&a, Some("payout".into())).await.unwrap();
        assert_eq!(s.lookup_address(&addr).await.unwrap(), a);
        assert_eq!(addr.kind().unwrap(), AccountKind::Model);
    }

    // ---- AP1: hold storage tests ------------------------------------------

    fn hold_create(from: &AccountId, amount: i64, key: &str) -> HoldCreateCommit {
        HoldCreateCommit {
            from: from.clone(),
            amount: mint_core::TransferAmount::new(Micros::new(amount)).unwrap(),
            idem_key: key.to_string(),
            op_hash: mint_core::op_hash(&serde_json::json!({
                "op": "hold.create", "from": from.as_str(), "amount": amount, "key": key,
            })),
            ref_: None,
            expires_at_ms: None,
        }
    }

    fn hold_capture(hold_id: &HoldId, amount: i64, to: &AccountId, key: &str) -> HoldCaptureCommit {
        HoldCaptureCommit {
            hold_id: hold_id.clone(),
            amount: mint_core::TransferAmount::new(Micros::new(amount)).unwrap(),
            to: to.clone(),
            idem_key: key.to_string(),
            op_hash: mint_core::op_hash(&serde_json::json!({
                "op": "hold.capture", "hold": hold_id.as_str(),
                "amount": amount, "to": to.as_str(), "key": key,
            })),
            ref_: None,
        }
    }

    fn hold_release(hold_id: &HoldId, key: &str) -> HoldReleaseCommit {
        HoldReleaseCommit {
            hold_id: hold_id.clone(),
            idem_key: key.to_string(),
            op_hash: mint_core::op_hash(&serde_json::json!({
                "op": "hold.release", "hold": hold_id.as_str(), "key": key,
            })),
            ref_: None,
        }
    }

    #[tokio::test]
    async fn hold_create_reserves_available_into_held() {
        let s = store();
        let a = account(&s, AccountKind::User, 10_000_000).await;
        let before = s.get_balance(&a).await.unwrap();

        let out = s
            .commit_hold_create(hold_create(&a, 4_000_000, "h1"))
            .await
            .unwrap();
        assert!(!out.idempotent_replay);
        assert_eq!(out.status, HoldStatus::Open);
        assert_eq!(out.captured_micros, Micros::zero());

        let after = s.get_balance(&a).await.unwrap();
        assert_eq!(after.available_micros, Micros::new(6_000_000)); // dropped by A
        assert_eq!(after.held_micros, Micros::new(4_000_000)); // rose by A
                                                               // available + held is unchanged.
        assert_eq!(
            after.available_micros.get() + after.held_micros.get(),
            before.available_micros.get() + before.held_micros.get()
        );
        assert_eq!(s.journal_len().await, 3); // 1 seed + 2 hold rows

        let holds = s.holds_snapshot();
        let rec = &holds[&out.hold_id];
        assert_eq!(rec.status, HoldStatus::Open);
        assert_eq!(rec.captured_micros, Micros::zero());
        assert_eq!(rec.amount_micros, Micros::new(4_000_000));
    }

    #[tokio::test]
    async fn hold_create_insufficient_funds_writes_nothing() {
        let s = store();
        let a = account(&s, AccountKind::User, 1_000_000).await;
        let len_before = s.journal_len().await;
        let bal_before = s.get_balance(&a).await.unwrap();

        let err = s
            .commit_hold_create(hold_create(&a, 5_000_000, "h1"))
            .await
            .unwrap_err();
        assert_eq!(err, MintError::InsufficientFunds);
        assert_eq!(s.journal_len().await, len_before);
        assert_eq!(s.get_balance(&a).await.unwrap(), bal_before);
        assert!(s.holds_snapshot().is_empty());
    }

    #[tokio::test]
    async fn hold_capture_full_settles_all_no_remainder() {
        let s = store();
        let a = account(&s, AccountKind::User, 10_000_000).await;
        let b = account(&s, AccountKind::Model, 0).await;
        let h = s
            .commit_hold_create(hold_create(&a, 4_000_000, "h1"))
            .await
            .unwrap();
        let len = s.journal_len().await;

        let out = s
            .commit_hold_capture(hold_capture(&h.hold_id, 4_000_000, &b, "c1"))
            .await
            .unwrap();
        assert_eq!(out.status, HoldStatus::Captured);
        assert_eq!(out.captured_micros, Micros::new(4_000_000));
        // exactly 2 rows (no remainder row).
        assert_eq!(s.journal_len().await, len + 2);

        let ha = s.get_balance(&a).await.unwrap();
        let hb = s.get_balance(&b).await.unwrap();
        assert_eq!(ha.held_micros, Micros::zero());
        assert_eq!(ha.available_micros, Micros::new(6_000_000)); // holder available unchanged
        assert_eq!(hb.available_micros, Micros::new(4_000_000)); // payee credited
        assert_eq!(s.holds_snapshot()[&h.hold_id].status, HoldStatus::Captured);
    }

    #[tokio::test]
    async fn hold_capture_partial_releases_remainder() {
        let s = store();
        let a = account(&s, AccountKind::User, 10_000_000).await;
        let b = account(&s, AccountKind::Model, 0).await;
        let h = s
            .commit_hold_create(hold_create(&a, 4_000_000, "h1"))
            .await
            .unwrap();
        let len = s.journal_len().await;

        let out = s
            .commit_hold_capture(hold_capture(&h.hold_id, 1_500_000, &b, "c1"))
            .await
            .unwrap();
        assert_eq!(out.captured_micros, Micros::new(1_500_000));
        // exactly 3 rows (remainder row present).
        assert_eq!(s.journal_len().await, len + 3);

        let ha = s.get_balance(&a).await.unwrap();
        let hb = s.get_balance(&b).await.unwrap();
        assert_eq!(ha.held_micros, Micros::zero());
        // holder gets remainder back: 6,000,000 + 2,500,000 = 8,500,000
        assert_eq!(ha.available_micros, Micros::new(8_500_000));
        assert_eq!(hb.available_micros, Micros::new(1_500_000));
    }

    #[tokio::test]
    async fn hold_capture_exceeds_held_rejected_hold_stays_open() {
        let s = store();
        let a = account(&s, AccountKind::User, 10_000_000).await;
        let b = account(&s, AccountKind::Model, 0).await;
        let h = s
            .commit_hold_create(hold_create(&a, 4_000_000, "h1"))
            .await
            .unwrap();
        let len = s.journal_len().await;

        let err = s
            .commit_hold_capture(hold_capture(&h.hold_id, 5_000_000, &b, "c1"))
            .await
            .unwrap_err();
        assert_eq!(err, MintError::CaptureExceedsHold);
        assert_eq!(s.journal_len().await, len); // nothing written
        assert_eq!(s.holds_snapshot()[&h.hold_id].status, HoldStatus::Open);

        // Can still be captured validly afterwards.
        let out = s
            .commit_hold_capture(hold_capture(&h.hold_id, 4_000_000, &b, "c2"))
            .await
            .unwrap();
        assert_eq!(out.status, HoldStatus::Captured);
    }

    #[tokio::test]
    async fn hold_capture_unknown_hold_is_not_found() {
        let s = store();
        let b = account(&s, AccountKind::Model, 0).await;
        let ghost = HoldId::new(Ulid::new());
        let len = s.journal_len().await;
        let err = s
            .commit_hold_capture(hold_capture(&ghost, 1, &b, "c1"))
            .await
            .unwrap_err();
        assert_eq!(err, MintError::HoldNotFound);
        assert_eq!(s.journal_len().await, len);
    }

    #[tokio::test]
    async fn hold_capture_to_holder_is_bad_account_id() {
        let s = store();
        let a = account(&s, AccountKind::User, 10_000_000).await;
        let h = s
            .commit_hold_create(hold_create(&a, 4_000_000, "h1"))
            .await
            .unwrap();
        let len = s.journal_len().await;
        let err = s
            .commit_hold_capture(hold_capture(&h.hold_id, 1_000_000, &a, "c1"))
            .await
            .unwrap_err();
        assert_eq!(err, MintError::BadAccountId);
        assert_eq!(s.journal_len().await, len);
        assert_eq!(s.holds_snapshot()[&h.hold_id].status, HoldStatus::Open);
    }

    #[tokio::test]
    async fn hold_release_frees_all_with_no_settlement() {
        let s = store();
        let a = account(&s, AccountKind::User, 10_000_000).await;
        let b = account(&s, AccountKind::Model, 0).await;
        let h = s
            .commit_hold_create(hold_create(&a, 4_000_000, "h1"))
            .await
            .unwrap();
        let len = s.journal_len().await;

        let out = s
            .commit_hold_release(hold_release(&h.hold_id, "r1"))
            .await
            .unwrap();
        assert_eq!(out.status, HoldStatus::Released);
        assert_eq!(out.captured_micros, Micros::zero());
        assert!(out.payee_balance.is_none());
        assert_eq!(s.journal_len().await, len + 2); // exactly 2 rows

        let ha = s.get_balance(&a).await.unwrap();
        assert_eq!(ha.held_micros, Micros::zero());
        assert_eq!(ha.available_micros, Micros::new(10_000_000)); // whole amount back
                                                                  // No payee balance changed anywhere.
        assert_eq!(
            s.get_balance(&b).await.unwrap().available_micros,
            Micros::zero()
        );
        assert_eq!(s.holds_snapshot()[&h.hold_id].status, HoldStatus::Released);
    }

    #[tokio::test]
    async fn terminal_distinct_close_matrix_is_not_found() {
        // capture-after-capture, release-after-capture, capture-after-release,
        // release-after-release — each a DISTINCT idem_key — => HoldNotFound,
        // writes nothing, terminal hold untouched.
        let s = store();
        let a = account(&s, AccountKind::User, 20_000_000).await;
        let b = account(&s, AccountKind::Model, 0).await;

        // Captured hold.
        let hc = s
            .commit_hold_create(hold_create(&a, 4_000_000, "hc"))
            .await
            .unwrap();
        s.commit_hold_capture(hold_capture(&hc.hold_id, 4_000_000, &b, "cap"))
            .await
            .unwrap();
        // Released hold.
        let hr = s
            .commit_hold_create(hold_create(&a, 3_000_000, "hr"))
            .await
            .unwrap();
        s.commit_hold_release(hold_release(&hr.hold_id, "rel"))
            .await
            .unwrap();

        let len = s.journal_len().await;
        let bal_a = s.get_balance(&a).await.unwrap();
        let bal_b = s.get_balance(&b).await.unwrap();

        // capture-after-capture
        assert_eq!(
            s.commit_hold_capture(hold_capture(&hc.hold_id, 1_000_000, &b, "cac"))
                .await
                .unwrap_err(),
            MintError::HoldNotFound
        );
        // release-after-capture
        assert_eq!(
            s.commit_hold_release(hold_release(&hc.hold_id, "rac"))
                .await
                .unwrap_err(),
            MintError::HoldNotFound
        );
        // capture-after-release
        assert_eq!(
            s.commit_hold_capture(hold_capture(&hr.hold_id, 1_000_000, &b, "car"))
                .await
                .unwrap_err(),
            MintError::HoldNotFound
        );
        // release-after-release
        assert_eq!(
            s.commit_hold_release(hold_release(&hr.hold_id, "rar"))
                .await
                .unwrap_err(),
            MintError::HoldNotFound
        );

        // Nothing written; terminal holds + balances untouched.
        assert_eq!(s.journal_len().await, len);
        assert_eq!(s.get_balance(&a).await.unwrap(), bal_a);
        assert_eq!(s.get_balance(&b).await.unwrap(), bal_b);
        assert_eq!(s.holds_snapshot()[&hc.hold_id].status, HoldStatus::Captured);
        assert_eq!(s.holds_snapshot()[&hr.hold_id].status, HoldStatus::Released);
    }

    #[tokio::test]
    async fn idempotent_replay_of_each_hold_op_is_a_no_op() {
        let s = store();
        let a = account(&s, AccountKind::User, 10_000_000).await;
        let b = account(&s, AccountKind::Model, 0).await;

        // create replay
        let c1 = s
            .commit_hold_create(hold_create(&a, 4_000_000, "h1"))
            .await
            .unwrap();
        let len1 = s.journal_len().await;
        let c2 = s
            .commit_hold_create(hold_create(&a, 4_000_000, "h1"))
            .await
            .unwrap();
        assert!(c2.idempotent_replay);
        assert_eq!(c1.hold_id, c2.hold_id); // SAME original hold id
        assert_eq!(s.journal_len().await, len1);

        // capture replay: payee credited exactly once
        let cap1 = s
            .commit_hold_capture(hold_capture(&c1.hold_id, 1_000_000, &b, "cap"))
            .await
            .unwrap();
        let len2 = s.journal_len().await;
        let bal_b = s.get_balance(&b).await.unwrap();
        let cap2 = s
            .commit_hold_capture(hold_capture(&c1.hold_id, 1_000_000, &b, "cap"))
            .await
            .unwrap();
        assert!(cap2.idempotent_replay);
        assert_eq!(cap1.first_seq, cap2.first_seq);
        assert_eq!(s.journal_len().await, len2);
        assert_eq!(s.get_balance(&b).await.unwrap(), bal_b); // credited once

        // release replay (on a fresh hold)
        let c3 = s
            .commit_hold_create(hold_create(&a, 2_000_000, "h3"))
            .await
            .unwrap();
        let r1 = s
            .commit_hold_release(hold_release(&c3.hold_id, "rel"))
            .await
            .unwrap();
        let len3 = s.journal_len().await;
        let r2 = s
            .commit_hold_release(hold_release(&c3.hold_id, "rel"))
            .await
            .unwrap();
        assert!(r2.idempotent_replay);
        assert_eq!(r1.holder_balance, r2.holder_balance);
        assert_eq!(s.journal_len().await, len3);
    }

    #[tokio::test]
    async fn same_key_different_body_conflicts_for_holds() {
        let s = store();
        let a = account(&s, AccountKind::User, 10_000_000).await;
        let b = account(&s, AccountKind::Model, 0).await;

        s.commit_hold_create(hold_create(&a, 4_000_000, "h1"))
            .await
            .unwrap();
        let len = s.journal_len().await;
        // create with same key, different amount
        assert_eq!(
            s.commit_hold_create(hold_create(&a, 5_000_000, "h1"))
                .await
                .unwrap_err(),
            MintError::IdempotencyConflict
        );
        assert_eq!(s.journal_len().await, len);

        let h = s
            .commit_hold_create(hold_create(&a, 2_000_000, "h2"))
            .await
            .unwrap();
        s.commit_hold_capture(hold_capture(&h.hold_id, 1_000_000, &b, "cap"))
            .await
            .unwrap();
        let len2 = s.journal_len().await;
        // capture with same key, different amount
        assert_eq!(
            s.commit_hold_capture(hold_capture(&h.hold_id, 2_000_000, &b, "cap"))
                .await
                .unwrap_err(),
            MintError::IdempotencyConflict
        );
        assert_eq!(s.journal_len().await, len2);
    }

    #[tokio::test]
    async fn rebuild_includes_held_after_mixed_sequence() {
        let s = store();
        let a = account(&s, AccountKind::User, 10_000_000).await;
        let b = account(&s, AccountKind::Model, 0).await;
        let c = account(&s, AccountKind::User, 5_000_000).await;

        // A mixed create/capture/release sequence + a plain transfer.
        let h1 = s
            .commit_hold_create(hold_create(&a, 4_000_000, "h1"))
            .await
            .unwrap();
        s.commit_hold_capture(hold_capture(&h1.hold_id, 1_500_000, &b, "c1"))
            .await
            .unwrap();
        let h2 = s
            .commit_hold_create(hold_create(&c, 2_000_000, "h2"))
            .await
            .unwrap();
        s.commit_hold_release(hold_release(&h2.hold_id, "r2"))
            .await
            .unwrap();
        s.commit_transfer(commit(&a, &b, 500_000, "t1"))
            .await
            .unwrap();
        // leave one hold Open too.
        let _h3 = s
            .commit_hold_create(hold_create(&a, 1_000_000, "h3"))
            .await
            .unwrap();

        let rebuilt = s.rebuild_balances().await.unwrap();
        let live = s.balances_snapshot();
        assert_eq!(
            rebuilt, live,
            "rebuild (incl held + updated_seq) must equal live bit-exact"
        );
        // sanity: some account currently holds funds.
        assert!(live.values().any(|b| b.held_micros.is_positive()));
    }

    #[tokio::test]
    async fn rebuild_holds_reconstructs_status_and_captured() {
        let s = store();
        let a = account(&s, AccountKind::User, 10_000_000).await;
        let b = account(&s, AccountKind::Model, 0).await;
        let h1 = s
            .commit_hold_create(hold_create(&a, 4_000_000, "h1"))
            .await
            .unwrap();
        s.commit_hold_capture(hold_capture(&h1.hold_id, 1_500_000, &b, "c1"))
            .await
            .unwrap();
        let h2 = s
            .commit_hold_create(hold_create(&a, 2_000_000, "h2"))
            .await
            .unwrap();
        s.commit_hold_release(hold_release(&h2.hold_id, "r2"))
            .await
            .unwrap();
        let h3 = s
            .commit_hold_create(hold_create(&a, 1_000_000, "h3"))
            .await
            .unwrap();

        let rebuilt = s.rebuild_holds().await.unwrap();
        // create/capture/release all reconstructed (ref_ carries None on rebuild).
        assert_eq!(rebuilt[&h1.hold_id].status, HoldStatus::Captured);
        assert_eq!(rebuilt[&h1.hold_id].captured_micros, Micros::new(1_500_000));
        assert_eq!(rebuilt[&h1.hold_id].amount_micros, Micros::new(4_000_000));
        assert_eq!(rebuilt[&h2.hold_id].status, HoldStatus::Released);
        assert_eq!(rebuilt[&h2.hold_id].captured_micros, Micros::zero());
        assert_eq!(rebuilt[&h3.hold_id].status, HoldStatus::Open);
    }

    #[test]
    fn capture_failpoint_is_all_or_nothing() {
        // Crash between capture computation and journal append: re-acquiring the
        // (poison-tolerant) lock must show journal, balances, and holds byte-
        // identical to pre-call — proving the single critical section. Uses a
        // plain runtime + a child thread so the panicking call has no ambient
        // runtime and its unwind is isolated.
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let s = Arc::new(store());
        let (h, _a, b) = rt.block_on(async {
            let a = account(&s, AccountKind::User, 10_000_000).await;
            let b = account(&s, AccountKind::Model, 0).await;
            let h = s
                .commit_hold_create(hold_create(&a, 4_000_000, "h1"))
                .await
                .unwrap();
            (h, a, b)
        });

        let journal_before = s.journal_snapshot();
        let balances_before = s.balances_snapshot();
        let holds_before = s.holds_snapshot();

        s.arm_capture_failpoint();
        let s2 = Arc::clone(&s);
        let hid = h.hold_id.clone();
        let bid = b.clone();
        // Run the fail-pointed capture on a child thread; joining it yields Err
        // because the closure panicked inside the critical section.
        let joined = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            rt.block_on(async move {
                let _ = s2
                    .commit_hold_capture(hold_capture(&hid, 4_000_000, &bid, "cap"))
                    .await;
            });
        })
        .join();
        assert!(
            joined.is_err(),
            "fail-point must panic inside the critical section"
        );

        // Nothing changed (poison-tolerant lock re-reads the untouched state).
        assert_eq!(s.journal_snapshot(), journal_before);
        assert_eq!(s.balances_snapshot(), balances_before);
        assert_eq!(s.holds_snapshot(), holds_before);

        // A retry with the SAME key now succeeds exactly once (retry-safe).
        rt.block_on(async {
            let out = s
                .commit_hold_capture(hold_capture(&h.hold_id, 4_000_000, &b, "cap"))
                .await
                .unwrap();
            assert_eq!(out.status, HoldStatus::Captured);
            assert_eq!(
                s.get_balance(&b).await.unwrap().available_micros,
                Micros::new(4_000_000)
            );
        });
    }

    // ---- AP2: receipt emission tests --------------------------------------

    use mint_receipts::{
        merkle_root as mk_root, reserve_proof_from_receipts, verify_log, Keyring, ReceiptHash,
        ReceiptOp,
    };

    #[tokio::test]
    async fn seed_emits_genesis_receipt_create_emits_none() {
        let s = store();
        // create with no seed emits nothing.
        let c = s.create_account(AccountKind::User, None).await.unwrap();
        assert!(s.receipts_snapshot().is_empty());

        // seed emits exactly one genesis receipt on that account.
        s.seed_account(&c, Micros::new(10_000_000)).await.unwrap();
        let snap = s.receipts_snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].body.op, ReceiptOp::Seed);
        assert_eq!(snap[0].body.account_seq, 0);
        assert_eq!(snap[0].body.prev_hash, ReceiptHash::ZERO);
        assert_eq!(snap[0].body.from, None);
        assert_eq!(snap[0].body.to, Some(c.clone()));
        assert_eq!(
            snap[0].body.balance_after_available_micros,
            Micros::new(10_000_000)
        );
        // A valid genesis hash = blake3(digest) and verifies under the keyring.
        let kr = Keyring::with_signer(s.signer.as_ref());
        assert!(verify_log(&snap, &kr).is_ok());
    }

    #[tokio::test]
    async fn transfer_emits_two_receipts_replay_safe() {
        let s = store();
        let a = account(&s, AccountKind::User, 10_000_000).await;
        let b = account(&s, AccountKind::Model, 0).await;
        let before = s.receipts_snapshot().len(); // one seed receipt (b unseeded)

        s.commit_transfer(commit(&a, &b, 4_000_000, "k1"))
            .await
            .unwrap();
        let snap = s.receipts_snapshot();
        assert_eq!(snap.len(), before + 2);
        // Debit side first (on `a`), then credit side (on `b`).
        let last2 = &snap[snap.len() - 2..];
        assert_eq!(last2[0].body.account_id, a);
        assert_eq!(last2[0].body.op, ReceiptOp::Transfer);
        assert_eq!(
            last2[0].body.balance_after_available_micros,
            Micros::new(6_000_000)
        );
        assert_eq!(last2[1].body.account_id, b);
        assert_eq!(
            last2[1].body.balance_after_available_micros,
            Micros::new(4_000_000)
        );

        // Idempotent replay appends ZERO receipts.
        for _ in 0..3 {
            s.commit_transfer(commit(&a, &b, 4_000_000, "k1"))
                .await
                .unwrap();
            assert_eq!(s.receipts_snapshot().len(), before + 2);
        }
    }

    #[tokio::test]
    async fn hold_ops_emit_expected_receipts() {
        let s = store();
        let a = account(&s, AccountKind::User, 10_000_000).await;
        let b = account(&s, AccountKind::Model, 0).await;
        let base = s.receipts_snapshot().len();

        // create ⇒ +1 on holder (held reflected).
        let h = s
            .commit_hold_create(hold_create(&a, 4_000_000, "h1"))
            .await
            .unwrap();
        let snap = s.receipts_snapshot();
        assert_eq!(snap.len(), base + 1);
        assert_eq!(snap[base].body.op, ReceiptOp::HoldCreate);
        assert_eq!(snap[base].body.account_id, a);
        assert_eq!(
            snap[base].body.balance_after_held_micros,
            Micros::new(4_000_000)
        );

        // partial capture ⇒ +2 (holder then payee), remainder does NOT add a 3rd.
        s.commit_hold_capture(hold_capture(&h.hold_id, 1_500_000, &b, "c1"))
            .await
            .unwrap();
        let snap = s.receipts_snapshot();
        assert_eq!(snap.len(), base + 3);
        assert_eq!(snap[base + 1].body.op, ReceiptOp::HoldCapture);
        assert_eq!(snap[base + 1].body.account_id, a); // holder
        assert_eq!(
            snap[base + 1].body.balance_after_held_micros,
            Micros::zero()
        );
        assert_eq!(snap[base + 2].body.account_id, b); // payee
        assert_eq!(
            snap[base + 2].body.balance_after_available_micros,
            Micros::new(1_500_000)
        );

        // release (fresh hold) ⇒ +1 on holder.
        let h2 = s
            .commit_hold_create(hold_create(&a, 1_000_000, "h2"))
            .await
            .unwrap();
        let len_after_create = s.receipts_snapshot().len();
        s.commit_hold_release(hold_release(&h2.hold_id, "r2"))
            .await
            .unwrap();
        let snap = s.receipts_snapshot();
        assert_eq!(snap.len(), len_after_create + 1);
        assert_eq!(snap.last().unwrap().body.op, ReceiptOp::HoldRelease);
        assert_eq!(snap.last().unwrap().body.account_id, a);
    }

    #[tokio::test]
    async fn replay_and_conflict_emit_no_receipt() {
        let s = store();
        let a = account(&s, AccountKind::User, 10_000_000).await;
        let b = account(&s, AccountKind::Model, 0).await;

        let h = s
            .commit_hold_create(hold_create(&a, 4_000_000, "h1"))
            .await
            .unwrap();
        s.commit_hold_capture(hold_capture(&h.hold_id, 2_000_000, &b, "c1"))
            .await
            .unwrap();
        let snap = s.receipts_snapshot();

        // Idempotent replays: no new receipts.
        s.commit_hold_create(hold_create(&a, 4_000_000, "h1"))
            .await
            .unwrap();
        s.commit_hold_capture(hold_capture(&h.hold_id, 2_000_000, &b, "c1"))
            .await
            .unwrap();
        assert_eq!(s.receipts_snapshot(), snap);

        // Same-key/different-body conflict: still no new receipt.
        assert_eq!(
            s.commit_hold_create(hold_create(&a, 9_000_000, "h1"))
                .await
                .unwrap_err(),
            MintError::IdempotencyConflict
        );
        assert_eq!(s.receipts_snapshot(), snap);
    }

    async fn mixed_workload(s: &InMemoryStore) -> i64 {
        // Returns the total seeded supply (µ$).
        let a = account(s, AccountKind::User, 10_000_000).await;
        let b = account(s, AccountKind::Model, 0).await;
        let c = account(s, AccountKind::User, 5_000_000).await;
        let _d = account(s, AccountKind::User, 0).await; // created, never touched

        s.commit_transfer(commit(&a, &b, 2_000_000, "t1"))
            .await
            .unwrap();
        let h1 = s
            .commit_hold_create(hold_create(&a, 3_000_000, "h1"))
            .await
            .unwrap();
        s.commit_hold_capture(hold_capture(&h1.hold_id, 1_000_000, &c, "cap1"))
            .await
            .unwrap(); // partial ⇒ remainder
        let h2 = s
            .commit_hold_create(hold_create(&c, 2_000_000, "h2"))
            .await
            .unwrap();
        s.commit_hold_release(hold_release(&h2.hold_id, "rel2"))
            .await
            .unwrap();
        let _open = s
            .commit_hold_create(hold_create(&b, 500_000, "h3"))
            .await
            .unwrap(); // left OPEN
        10_000_000 + 5_000_000
    }

    #[tokio::test]
    async fn live_equals_rebuild_receipts_bit_exact() {
        let s = store();
        mixed_workload(&s).await;
        let rebuilt = s.rebuild_receipts().await.unwrap();
        assert_eq!(
            rebuilt.global(),
            s.receipts_snapshot().as_slice(),
            "rebuilt receipt log must equal the live log byte-for-byte"
        );
    }

    #[tokio::test]
    async fn merkle_and_reserve_end_to_end() {
        let s = store();
        let supply = mixed_workload(&s).await;

        // store.merkle_root() == merkle over the exported snapshot.
        let snap = s.receipts_snapshot();
        assert_eq!(s.merkle_root(), mk_root(&snap));

        // Reserve proof from receipts equals the seeded supply.
        let proof = reserve_proof_from_receipts(&snap, Micros::new(supply));
        assert!(proof.ok, "backed {:?} != supply", proof.backed_micros);
        assert_eq!(proof.backed_micros, Micros::new(supply));

        // The whole live log verifies offline under the signer's public key.
        let kr = Keyring::with_signer(s.signer.as_ref());
        assert!(verify_log(&snap, &kr).is_ok());
    }

    #[test]
    fn capture_failpoint_receipts_all_or_nothing() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        let s = Arc::new(store());
        let (h, _a, b) = rt.block_on(async {
            let a = account(&s, AccountKind::User, 10_000_000).await;
            let b = account(&s, AccountKind::Model, 0).await;
            let h = s
                .commit_hold_create(hold_create(&a, 4_000_000, "h1"))
                .await
                .unwrap();
            (h, a, b)
        });

        let receipts_before = s.receipts_snapshot();

        s.arm_capture_failpoint();
        let s2 = Arc::clone(&s);
        let hid = h.hold_id.clone();
        let bid = b.clone();
        let joined = std::thread::spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .build()
                .unwrap();
            rt.block_on(async move {
                let _ = s2
                    .commit_hold_capture(hold_capture(&hid, 4_000_000, &bid, "cap"))
                    .await;
            });
        })
        .join();
        assert!(joined.is_err());

        // Receipts unchanged after the panicking capture (all-or-nothing).
        assert_eq!(s.receipts_snapshot(), receipts_before);

        // A same-key retry emits exactly the holder+payee pair once.
        rt.block_on(async {
            s.commit_hold_capture(hold_capture(&h.hold_id, 4_000_000, &b, "cap"))
                .await
                .unwrap();
        });
        assert_eq!(s.receipts_snapshot().len(), receipts_before.len() + 2);
    }
}
