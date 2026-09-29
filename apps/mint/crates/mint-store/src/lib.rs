//! `mint-store` — the persistence seam.
//!
//! Defines the async [`LedgerStore`] trait (Send + Sync) that `mint-ledger`
//! depends on, plus the AP0 [`InMemoryStore`] impl with an ATOMIC
//! reserve-and-append `commit_transfer` (one critical section = no
//! check-then-append race). The trait boundary lets a future Postgres/sqlx impl
//! slot in with zero changes to `mint-ledger`.
//!
//! - [`store_trait`] — the async [`LedgerStore`] trait boundary.
//! - [`memory`]      — [`InMemoryStore`] (single-lock atomic `commit_transfer`).
//! - [`journal`]     — append-only [`JournalEntry`] row shape + [`OpKind`].
//! - [`balance`]     — rebuildable [`Balance`] projection.
//! - [`clock`]       — injectable [`Clock`] (`SystemClock` / `FixedClock`).

pub mod balance;
pub mod clock;
pub mod journal;
pub mod memory;
pub mod mirror;
pub mod operator;
pub mod reporting;
pub mod seller;
pub mod store_trait;

pub use balance::Balance;
pub use clock::{Clock, FixedClock, SystemClock};
pub use journal::{Bucket, JournalEntry, OpKind};
pub use memory::{ait_to_micros, InMemoryStore};
pub use mirror::{MemoryMirror, MirrorDelivery, MirrorEvent, MirrorKind, MirrorStore};
pub use seller::{
    ConnectToken, EntitlementDelivery, EntitlementRecord, MemorySellerStore, SellerProductRecord, SellerRecord,
    SellerStore,
};
pub use operator::{
    AccountStatus, ApiKeyRecord, DepositReversal, PayerKeyRecord, ProviderReport,
    ProviderReportLine, ReportLineKind, ReserveMovement, ReversalStatus,
};
pub use reporting::{
    provider_tag_of, AccountSummary, PayoutQuery, PayoutRecord, PayoutRecordStatus, ReceiptQuery,
    ReserveMovementTotal,
};
pub use store_trait::{
    AllowanceGrantCommit, AllowanceGrantOutcome, AllowanceRecord, AllowanceRevokeCommit,
    AllowanceScheme, AllowanceScope, AllowanceSpendCommit, AllowanceSpendOutcome, AllowanceWindow,
    CommitOutcome, HoldCaptureCommit, HoldCreateCommit, HoldOutcome, HoldRecord, HoldReleaseCommit,
    HoldStatus, LedgerStore, MintCommit, MintOutcome, RedeemCommit, RedeemOutcome,
    StreamCloseCommit, StreamCloseOutcome, StreamOpenCommit, StreamOpenOutcome, StreamRecord,
    StreamStatus, StreamTickCommit, StreamTickOutcome, StreamUnit, TransferCommit,
};

// Re-export the receipt vocabulary so downstream crates (and tests) can build a
// store with a custom signer and verify its log without a direct dep.
pub use mint_receipts::{
    merkle_root, reserve_proof, reserve_proof_from_receipts, verify_log, Ed25519Signer,
    FixedSigner, Keyring, Receipt, ReceiptHash, ReceiptLog, ReserveProof, Signer, VerifyError,
    DEFAULT_KEY_ID, FIXED_SEED,
};
