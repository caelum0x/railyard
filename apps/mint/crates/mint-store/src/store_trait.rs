//! The persistence seam: the async [`LedgerStore`] trait plus the
//! [`TransferCommit`] input and [`CommitOutcome`] result for the atomic
//! double-entry commit.
//!
//! `mint-ledger` depends only on this trait, so a future Postgres/sqlx impl
//! slots in with zero changes to `mint-ledger` — it expresses the same
//! atomicity as one SQL transaction with row locks.

use std::collections::HashMap;

use async_trait::async_trait;
use mint_core::{
    AccountId, AccountKind, Address, AllowanceId, HoldId, Micros, MintError, OpHash, StreamId,
    TransferAmount,
};
use mint_receipts::{Receipt, ReceiptHash, ReceiptLog};
use serde::{Deserialize, Serialize};

use crate::balance::Balance;

/// The validated input to the atomic transfer commit. Built by `mint-ledger`
/// after boundary validation (amount > 0, no self-transfer) and `op_hash`
/// computation.
#[derive(Clone, Debug)]
pub struct TransferCommit {
    /// Debited account.
    pub from: AccountId,
    /// Credited account.
    pub to: AccountId,
    /// Strictly-positive amount.
    pub amount: TransferAmount,
    /// Idempotency key.
    pub idem_key: String,
    /// Canonical op hash (identifies the body for idempotency-conflict checks).
    pub op_hash: OpHash,
    /// Optional reference.
    pub ref_: Option<String>,
}

/// The result of a committed (or idempotently replayed) transfer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitOutcome {
    /// `seq` of the debit row (the first of the two appended rows).
    pub first_seq: u64,
    /// Post-commit balance of the sender.
    pub from_balance: Balance,
    /// Post-commit balance of the receiver.
    pub to_balance: Balance,
    /// `true` when this call replayed a prior identical commit (no new rows).
    pub idempotent_replay: bool,
}

/// The lifecycle of a hold. `Open` is the only mutable state; `Captured` and
/// `Released` are terminal. A distinct (non-replay) close of a terminal hold is
/// [`MintError::HoldNotFound`].
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum HoldStatus {
    /// Reservation is live and may be captured or released exactly once.
    Open,
    /// Terminal: settled (fully or partially) to a payee; remainder released.
    Captured,
    /// Terminal: freed with no settlement (the success-only failure path).
    Released,
}

/// The authoritative live record for a hold (mirrors the SQL `holds` table).
/// Owned by the store; a rebuildable projection of the hold journal rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoldRecord {
    /// The hold id (`hold_<ULID>`), minted by the store on create.
    pub id: HoldId,
    /// The account whose funds are reserved.
    pub account_id: AccountId,
    /// The whole reservation, immutable once open.
    pub amount_micros: Micros,
    /// Settled amount: `0` while `Open`, the captured amount on capture, stays
    /// `0` on release.
    pub captured_micros: Micros,
    /// Lifecycle state.
    pub status: HoldStatus,
    /// Optional free-form reference carried from the create request.
    pub ref_: Option<String>,
}

/// Validated input to `commit_hold_create`. No `hold_id` — the store mints it
/// inside the critical section (like `seq` and `AccountId`), so an idempotent
/// replay returns the SAME original hold id.
#[derive(Clone, Debug)]
pub struct HoldCreateCommit {
    /// Account whose available balance is reserved.
    pub from: AccountId,
    /// Strictly-positive reservation amount.
    pub amount: TransferAmount,
    /// Idempotency key.
    pub idem_key: String,
    /// Canonical op hash (excludes hold_id; identifies the body).
    pub op_hash: OpHash,
    /// Optional reference.
    pub ref_: Option<String>,
}

/// Validated input to `commit_hold_capture`.
#[derive(Clone, Debug)]
pub struct HoldCaptureCommit {
    /// The hold to settle.
    pub hold_id: HoldId,
    /// Strictly-positive capture amount (must be `<= hold.amount`).
    pub amount: TransferAmount,
    /// The payee credited with the captured amount.
    pub to: AccountId,
    /// Idempotency key.
    pub idem_key: String,
    /// Canonical op hash (includes hold_id).
    pub op_hash: OpHash,
    /// Optional reference.
    pub ref_: Option<String>,
}

/// Validated input to `commit_hold_release`.
#[derive(Clone, Debug)]
pub struct HoldReleaseCommit {
    /// The hold to free.
    pub hold_id: HoldId,
    /// Idempotency key.
    pub idem_key: String,
    /// Canonical op hash (includes hold_id).
    pub op_hash: OpHash,
    /// Optional reference.
    pub ref_: Option<String>,
}

/// The shared result of a committed (or idempotently replayed) hold op.
/// `payee_balance` is `Some` only for a capture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoldOutcome {
    /// The hold id (minted on create; echoed on capture/release).
    pub hold_id: HoldId,
    /// `seq` of the first appended row for this op.
    pub first_seq: u64,
    /// Post-op balance of the holder (the account owning the reservation).
    pub holder_balance: Balance,
    /// Post-op balance of the payee (capture only; `None` for create/release).
    pub payee_balance: Option<Balance>,
    /// The hold's lifecycle state after the op.
    pub status: HoldStatus,
    /// The hold's captured amount after the op.
    pub captured_micros: Micros,
    /// `true` when this call replayed a prior identical commit (no new rows).
    pub idempotent_replay: bool,
}

// ---- Allowances (AITP-402 §4 — delegated spend budget, AP3) ---------------

/// Which settlement primitive a delegated [`AllowanceSpendCommit`] drives. Kept
/// distinct from `mint-protocol`'s wire `Scheme` (which carries the `aitp-hold`
/// /`aitp-transfer` renames) so `mint-store` stays free of any protocol
/// dependency — allowance is a ledger verb, not a wire handshake.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum AllowanceScheme {
    /// Auto-settle by debiting the granter and crediting `to` (a transfer).
    Transfer,
    /// Auto-settle by reserving from the granter (a `hold.create`), later
    /// captured/released by the caller.
    Hold,
}

/// The reset policy of an allowance's spend budget.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum AllowanceWindow {
    /// The budget never resets: `spent_micros` accumulates for the allowance's
    /// whole life (a single cumulative cap).
    PerCall,
    /// A rolling window: `spent_micros` resets to zero once
    /// `now - window_start_ms >= window_ms`.
    Rolling {
        /// Window length in milliseconds.
        window_ms: i64,
    },
}

/// Optional restriction on where an allowance may be spent. `None` fields are
/// unrestricted; a mismatch is [`MintError::AllowanceExceeded`].
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct AllowanceScope {
    /// If set, the spend's payee (`to`) must equal this account id string.
    pub payee: Option<String>,
    /// If set, the spend's `ref_` must start with this prefix.
    pub resource_prefix: Option<String>,
}

/// The authoritative live record for a delegated allowance (SPEC §4). Owned by
/// the store; the analog of [`HoldRecord`] for the allowance verb.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllowanceRecord {
    /// The allowance id (`alw_<ULID>`), minted by the store on grant.
    pub id: AllowanceId,
    /// The account whose funds the delegate may spend.
    pub granter: AccountId,
    /// Opaque key identifying the delegate the budget was granted to.
    pub to_key: String,
    /// The spend cap for the current window.
    pub limit_micros: Micros,
    /// Amount spent in the current window (reset per [`AllowanceWindow`]).
    pub spent_micros: Micros,
    /// Reset policy.
    pub window: AllowanceWindow,
    /// The unix-ms start of the current window (set to `clock.now()` on grant
    /// and on each rolling reset).
    pub window_start_ms: i64,
    /// Optional payee/resource restriction.
    pub scope: Option<AllowanceScope>,
    /// Once revoked, every further spend is [`MintError::AllowanceExceeded`].
    pub revoked: bool,
}

/// Validated input to `commit_allowance_grant`. No `allowance_id` — the store
/// mints it inside the critical section (like `HoldId`), so an idempotent replay
/// returns the SAME original id.
#[derive(Clone, Debug)]
pub struct AllowanceGrantCommit {
    /// Account delegating the budget (must exist).
    pub granter: AccountId,
    /// Opaque delegate key.
    pub to_key: String,
    /// Strictly-positive spend cap.
    pub limit: TransferAmount,
    /// Reset policy.
    pub window: AllowanceWindow,
    /// Optional payee/resource restriction.
    pub scope: Option<AllowanceScope>,
    /// Idempotency key.
    pub idem_key: String,
    /// Canonical op hash (excludes allowance_id; identifies the body).
    pub op_hash: OpHash,
}

/// The result of a committed (or idempotently replayed) allowance grant.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllowanceGrantOutcome {
    /// The minted (or replayed) allowance id.
    pub allowance_id: AllowanceId,
    /// `true` when this call replayed a prior identical grant (no new record).
    pub idempotent_replay: bool,
}

/// Validated input to `commit_allowance_spend`.
#[derive(Clone, Debug)]
pub struct AllowanceSpendCommit {
    /// The allowance to draw against.
    pub allowance_id: AllowanceId,
    /// Strictly-positive spend amount.
    pub amount: TransferAmount,
    /// The payee credited (transfer) or the eventual capture target (hold).
    pub to: AccountId,
    /// Whether to auto-settle via transfer or hold.
    pub scheme: AllowanceScheme,
    /// Optional free-form reference (matched against `scope.resource_prefix`).
    pub ref_: Option<String>,
    /// Idempotency key.
    pub idem_key: String,
    /// Canonical op hash (identifies the body for idempotency-conflict checks).
    pub op_hash: OpHash,
}

/// The result of a committed (or idempotently replayed) allowance spend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllowanceSpendOutcome {
    /// The allowance drawn against.
    pub allowance_id: AllowanceId,
    /// Remaining budget in the current window after this spend.
    pub remaining_micros: Micros,
    /// `seq` of the first journal row appended by the auto-settlement.
    pub settlement_seq: u64,
    /// For a `Hold` scheme, the hold minted against the granter (`None` for
    /// `Transfer`).
    pub hold_id: Option<HoldId>,
    /// `true` when this call replayed a prior identical spend (no money moved).
    pub idempotent_replay: bool,
}

/// Validated input to `commit_allowance_revoke`.
#[derive(Clone, Debug)]
pub struct AllowanceRevokeCommit {
    /// The allowance to revoke.
    pub allowance_id: AllowanceId,
    /// Idempotency key.
    pub idem_key: String,
    /// Canonical op hash.
    pub op_hash: OpHash,
}

// ---- Streams (AITP-402 §4 — metered payment channels, AP4) ----------------

/// The lifecycle of a stream. `Open` is the only mutable state; `Closed` is
/// terminal (mirrors [`HoldStatus`]). A tick/close of a `Closed` stream is
/// [`MintError::StreamClosed`].
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum StreamStatus {
    /// Live: may be ticked (monotonic seq) or closed exactly once.
    Open,
    /// Terminal: final tick settled and the unused remainder released.
    Closed,
}

/// The metering granularity of a stream. Purely descriptive — settlement is
/// always `units * rate` regardless of unit — but carried so a channel's
/// intended cadence (per-second vs per-token) is chain-visible.
#[derive(Copy, Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum StreamUnit {
    /// Clock-driven metering (the deterministic default).
    Second,
    /// Opaque unit metering (e.g. per token / per request).
    Unit,
}

/// The authoritative live record for a stream (a rebuildable projection of the
/// stream journal rows keyed by `ref_ = stream_id`). Mirrors [`HoldRecord`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamRecord {
    /// The stream id (`strm_<ULID>`), minted by the store on open.
    pub id: StreamId,
    /// The payer whose funds back the channel (cap reserved on open).
    pub from_id: AccountId,
    /// The payee credited by each tick.
    pub to_id: AccountId,
    /// Per-unit price in microdollars.
    pub rate_micros: Micros,
    /// Metering granularity.
    pub unit: StreamUnit,
    /// The whole reservation, immutable once open.
    pub cap_micros: Micros,
    /// Amount settled to the payee so far (`0` on open; grows per tick).
    pub settled_micros: Micros,
    /// The highest applied tick `seq` (strictly increasing).
    pub last_seq: u64,
    /// Lifecycle state.
    pub status: StreamStatus,
    /// Optional free-form reference carried from the open request.
    pub ref_: Option<String>,
}

/// Validated input to `commit_stream_open`. No `stream_id` — the store mints it
/// inside the critical section (like `HoldCreateCommit`), so an idempotent replay
/// returns the SAME original stream id.
#[derive(Clone, Debug)]
pub struct StreamOpenCommit {
    /// The payer whose available balance is reserved for the whole cap.
    pub from: AccountId,
    /// The payee each tick settles to.
    pub to: AccountId,
    /// Strictly-positive per-unit rate.
    pub rate: TransferAmount,
    /// Strictly-positive cap (the whole up-front reservation).
    pub cap: TransferAmount,
    /// Metering granularity.
    pub unit: StreamUnit,
    /// Idempotency key.
    pub idem_key: String,
    /// Canonical op hash (excludes stream_id; identifies the body).
    pub op_hash: OpHash,
    /// Optional reference.
    pub ref_: Option<String>,
}

/// Validated input to `commit_stream_tick`.
#[derive(Clone, Debug)]
pub struct StreamTickCommit {
    /// The stream to settle against.
    pub stream_id: StreamId,
    /// Units metered this tick (`units * rate` is settled).
    pub units: u64,
    /// Strictly-increasing tick sequence number.
    pub seq: u64,
    /// Idempotency key.
    pub idem_key: String,
    /// Canonical op hash (unique per tick — folds stream_id + seq + units).
    pub op_hash: OpHash,
    /// Optional reference.
    pub ref_: Option<String>,
}

/// Validated input to `commit_stream_close`. `units` is the final tick (may be
/// `0` to just release the remainder).
#[derive(Clone, Debug)]
pub struct StreamCloseCommit {
    /// The stream to close.
    pub stream_id: StreamId,
    /// Final-tick units (may be `0`).
    pub units: u64,
    /// The final-tick sequence number (strictly `> last_seq` when `units > 0`).
    pub seq: u64,
    /// Idempotency key.
    pub idem_key: String,
    /// Canonical op hash.
    pub op_hash: OpHash,
    /// Optional reference.
    pub ref_: Option<String>,
}

/// The result of a committed (or replayed) `stream.open`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamOpenOutcome {
    /// The minted (or replayed) stream id.
    pub stream_id: StreamId,
    /// `seq` of the first appended row.
    pub first_seq: u64,
    /// Post-op balance of the payer (available dropped by cap, held rose by cap).
    pub holder_balance: Balance,
    /// `true` when this replayed a prior identical open (no new rows).
    pub idempotent_replay: bool,
}

/// The result of a committed (or replayed) `stream.tick`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamTickOutcome {
    /// The stream ticked.
    pub stream_id: StreamId,
    /// The applied tick `seq`.
    pub tick_seq: u64,
    /// Cumulative settled amount after this tick.
    pub settled_total_micros: Micros,
    /// Post-op payer balance.
    pub from_balance: Balance,
    /// Post-op payee balance.
    pub to_balance: Balance,
    /// `true` when this replayed a prior identical tick (no money moved).
    pub idempotent_replay: bool,
}

/// The result of a committed (or replayed) `stream.close`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamCloseOutcome {
    /// The stream closed.
    pub stream_id: StreamId,
    /// Cumulative settled amount (incl. any final tick).
    pub settled_total_micros: Micros,
    /// The unused remainder released back to the payer's available.
    pub released_micros: Micros,
    /// Post-op payer balance.
    pub from_balance: Balance,
    /// Post-op payee balance (reflects any final tick; equals prior if `units==0`).
    pub to_balance: Balance,
    /// `true` when this replayed a prior identical close.
    pub idempotent_replay: bool,
}

// ---- Reserve (AITP-402 §6/§7 — deposit-gated mint / KYC-gated redeem) ------

/// Validated input to `commit_mint`. Credits `to.available` AND `reserve_micros`
/// by `amount` in one critical section (both rise → `Σ(avail+held) == reserve`
/// preserved). Deposit gating happens ABOVE the store, in `mint-reserve`.
#[derive(Clone, Debug)]
pub struct MintCommit {
    /// The account credited with freshly-minted AIT.
    pub to: AccountId,
    /// Strictly-positive mint amount.
    pub amount: TransferAmount,
    /// The confirmed deposit this mint is gated on (stored as the row `ref_`).
    pub deposit_ref: String,
    /// Idempotency key (carries the deposit_ref so a deposit mints exactly once).
    pub idem_key: String,
    /// Canonical op hash.
    pub op_hash: OpHash,
}

/// Validated input to `commit_redeem`. Debits `from.available` AND
/// `reserve_micros` by `amount` atomically (both fall). KYC/limit gating happens
/// ABOVE the store, in `mint-reserve`.
#[derive(Clone, Debug)]
pub struct RedeemCommit {
    /// The account whose AIT is burned.
    pub from: AccountId,
    /// Strictly-positive redeem amount.
    pub amount: TransferAmount,
    /// The payout this redeem funds (stored as the row `ref_`).
    pub payout_ref: String,
    /// Idempotency key.
    pub idem_key: String,
    /// Canonical op hash.
    pub op_hash: OpHash,
}

/// The result of a committed (or replayed) `mint`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MintOutcome {
    /// `seq` of the appended mint row.
    pub first_seq: u64,
    /// Post-mint balance of the credited account.
    pub to_balance: Balance,
    /// The in-store reserve after this mint.
    pub reserve_after_micros: Micros,
    /// `true` when this replayed a prior identical mint (no second credit).
    pub idempotent_replay: bool,
}

/// The result of a committed (or replayed) `redeem`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RedeemOutcome {
    /// `seq` of the appended redeem row.
    pub first_seq: u64,
    /// Post-burn balance of the debited account.
    pub from_balance: Balance,
    /// The in-store reserve after this redeem.
    pub reserve_after_micros: Micros,
    /// `true` when this replayed a prior identical redeem (no second burn).
    pub idempotent_replay: bool,
}

/// The store boundary. All mutation, locking, atomicity, idempotency, journal
/// append, and projection logic lives behind this trait.
#[async_trait]
pub trait LedgerStore: Send + Sync {
    /// Create a fresh account (zero balance) and return its id.
    async fn create_account(
        &self,
        kind: AccountKind,
        owner_ref: Option<String>,
    ) -> Result<AccountId, MintError>;

    // NOTE: `seed_account` is intentionally NOT part of this trait. It mints a
    // single-sided opening balance WITHOUT a backing reserve leg, so exposing it
    // on the store trait would let any `S: LedgerStore` consumer (notably the AP4
    // `ReserveLedger`) print unbacked AIT and break `Σ(available + held) ==
    // reserve`. It survives only as a concrete AP0/test bootstrap primitive on
    // `InMemoryStore` (an inherent method), unreachable through the trait surface
    // that production reserve deployments depend on.

    /// Create a stable receive address for an account.
    async fn create_address(
        &self,
        account_id: &AccountId,
        purpose: Option<String>,
    ) -> Result<Address, MintError>;

    /// Resolve an address back to its owning account.
    async fn lookup_address(&self, addr: &Address) -> Result<AccountId, MintError>;

    /// THE atomic seam: idempotency lookup + both-account existence + balance
    /// check + non-negative guard + append of two signed journal rows +
    /// projection apply — all under one critical section, all-or-nothing.
    async fn commit_transfer(&self, commit: TransferCommit) -> Result<CommitOutcome, MintError>;

    /// Atomic hold create: reserve `amount` from `from` (available→held), mint a
    /// `HoldId`, open a `HoldRecord`. Same critical-section, all-or-nothing
    /// discipline as `commit_transfer`.
    async fn commit_hold_create(&self, commit: HoldCreateCommit) -> Result<HoldOutcome, MintError>;

    /// Atomic hold capture: settle `amount` (<= held) to `to`, release the
    /// remainder to the holder, close the hold `Captured`.
    async fn commit_hold_capture(
        &self,
        commit: HoldCaptureCommit,
    ) -> Result<HoldOutcome, MintError>;

    /// Atomic hold release: free the whole reservation back to available with NO
    /// settlement, close the hold `Released` (the success-only failure path).
    async fn commit_hold_release(
        &self,
        commit: HoldReleaseCommit,
    ) -> Result<HoldOutcome, MintError>;

    /// Atomic allowance grant: validate the granter exists, mint an
    /// [`AllowanceId`], and open an [`AllowanceRecord`] with `window_start_ms`
    /// set to the clock. Idempotent by `idem_key` (same original id on replay).
    async fn commit_allowance_grant(
        &self,
        commit: AllowanceGrantCommit,
    ) -> Result<AllowanceGrantOutcome, MintError>;

    /// Atomic allowance spend: load the record, roll its window, enforce
    /// scope + remaining budget, then auto-settle (transfer or hold) AND bump
    /// `spent_micros` — all under ONE critical section so concurrent spends can
    /// never both pass the check and overspend. [`MintError::AllowanceExceeded`]
    /// on any budget/scope/revocation failure, with no money moved.
    async fn commit_allowance_spend(
        &self,
        commit: AllowanceSpendCommit,
    ) -> Result<AllowanceSpendOutcome, MintError>;

    /// Atomic allowance revoke: mark the record revoked so further spends fail
    /// [`MintError::AllowanceExceeded`]. Idempotent by `idem_key`.
    async fn commit_allowance_revoke(
        &self,
        commit: AllowanceRevokeCommit,
    ) -> Result<AllowanceId, MintError>;

    /// Read a live allowance record (for tests / introspection).
    async fn get_allowance(&self, id: &AllowanceId) -> Result<AllowanceRecord, MintError>;

    /// Atomic stream open: reserve the whole `cap` from `from` (available→held),
    /// mint a [`StreamId`], open a [`StreamRecord`]. Conservation-neutral (the
    /// reserve is untouched). Idempotent by `idem_key` (same original id on
    /// replay).
    async fn commit_stream_open(
        &self,
        commit: StreamOpenCommit,
    ) -> Result<StreamOpenOutcome, MintError>;

    /// Atomic stream tick: settle `units*rate` held(payer)→available(payee),
    /// enforcing `status == Open`, `seq > last_seq`, and
    /// `settled + amount <= cap`. No partial application on any failure.
    async fn commit_stream_tick(
        &self,
        commit: StreamTickCommit,
    ) -> Result<StreamTickOutcome, MintError>;

    /// Atomic stream close: optional final tick (same guards as `tick` when
    /// `units > 0`) then release the unused remainder held(payer)→available(payer)
    /// and mark the stream `Closed` (terminal).
    async fn commit_stream_close(
        &self,
        commit: StreamCloseCommit,
    ) -> Result<StreamCloseOutcome, MintError>;

    /// Read a live stream record (for tests / introspection).
    async fn get_stream(&self, id: &StreamId) -> Result<StreamRecord, MintError>;

    /// Rebuild the streams table by folding the stream journal rows (keyed by
    /// `ref_ = stream_id`): `StreamOpen` ⇒ `Open` + cap, `StreamTick` ⇒
    /// `settled += payee-leg amount`, `StreamClose` ⇒ `Closed`.
    async fn rebuild_streams(&self) -> Result<HashMap<StreamId, StreamRecord>, MintError>;

    /// Atomic mint: credit `to.available += amount` AND `reserve_micros += amount`
    /// in one critical section (both rise → conservation preserved). Appends one
    /// `OpKind::Mint` row + one `ReceiptOp::Mint` receipt. Deposit gating is the
    /// caller's (`mint-reserve`) responsibility.
    async fn commit_mint(&self, commit: MintCommit) -> Result<MintOutcome, MintError>;

    /// Atomic redeem: debit `from.available -= amount` (checked non-negative →
    /// [`MintError::InsufficientFunds`]) AND `reserve_micros -= amount` (checked
    /// `reserve >= amount` → [`MintError::ReserveInsufficient`]) atomically.
    /// Appends one `OpKind::Redeem` row + one `ReceiptOp::Redeem` receipt.
    async fn commit_redeem(&self, commit: RedeemCommit) -> Result<RedeemOutcome, MintError>;

    /// The in-store reserve total (µ$). The §6 solvency proof uses this as
    /// `supply`, so `Σ(available + held) == reserve` is directly assertable.
    async fn reserve_micros(&self) -> Micros;

    /// Read an account's live projected balance.
    async fn get_balance(&self, account_id: &AccountId) -> Result<Balance, MintError>;

    /// Rebuild every account's balance by folding the journal from `seq 0`.
    async fn rebuild_balances(&self) -> Result<HashMap<AccountId, Balance>, MintError>;

    /// Rebuild the holds table by folding the hold journal rows (keyed by
    /// `ref_ = hold_id`): `HoldCreate` ⇒ `Open`, `HoldCapture` ⇒ `Captured`
    /// (captured amount from the payee row), `HoldRelease` ⇒ `Released`.
    async fn rebuild_holds(&self) -> Result<HashMap<HoldId, HoldRecord>, MintError>;

    /// Number of journal rows appended so far.
    async fn journal_len(&self) -> u64;

    /// Rebuild the entire receipt log by folding the journal per account and
    /// re-appending drafts through the store's signer. The result must equal the
    /// live `State.receipts` bit-exact (the AP2 tamper-evidence gate: receipts
    /// are a foldable projection of the journal).
    async fn rebuild_receipts(&self) -> Result<ReceiptLog, MintError>;

    /// Snapshot the full signed receipt log in global append order (for offline
    /// export / audit).
    async fn snapshot_receipts(&self) -> Vec<Receipt>;

    /// The Merkle audit root over the whole live receipt log (§6 audit).
    async fn receipt_merkle_root(&self) -> ReceiptHash;
}
