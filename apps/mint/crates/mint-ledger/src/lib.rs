//! `mint-ledger` — thin orchestration / public API, generic over
//! `S: LedgerStore`, so it is store-agnostic.
//!
//! Turns high-level intents (`create_account`, `create_address`, `transfer`,
//! `read_balance`) into validated store calls. Contains NO locking/persistence
//! logic (the store's job) and NO holds/streams/receipts/AITP-402 (later phases).
//!
//! - [`ledger`]    — [`Ledger`] public API surface.
//! - [`transfer`]  — [`TransferRequest`] / [`TransferReceipt`] boundary DTOs.
//! - [`hold`]      — AP1 hold/escrow boundary DTOs.
//! - [`allowance`] — AP3 delegated-budget (`allowance.grant`/`spend`) DTOs.

pub mod allowance;
pub mod hold;
pub mod ledger;
pub mod transfer;

pub use allowance::{AllowanceGrantRequest, AllowanceSpendRequest, SpendReceipt};
pub use hold::{HoldCaptureRequest, HoldCreateRequest, HoldReceipt, HoldReleaseRequest};
pub use ledger::Ledger;
pub use transfer::{TransferReceipt, TransferRequest};

// Re-export the store-facing vocabulary so downstream callers (and the property
// tests) can build a ledger without depending on `mint-store` directly.
pub use mint_core::{
    AccountId, AccountKind, Address, AllowanceId, HoldId, Micros, MintError, MICROS_PER_AIT,
};
pub use mint_store::{
    AllowanceRecord, AllowanceScheme, AllowanceScope, AllowanceWindow, Balance, Bucket, HoldRecord,
    HoldStatus, InMemoryStore, LedgerStore, OpKind,
};

// Re-export the receipt/verification surface (AP2 §6) so clients can verify a
// full history offline (no HTTP — that is AP-later).
pub use mint_receipts::{
    merkle_root, reserve_proof, reserve_proof_from_receipts, verify_chain, verify_log,
    Ed25519Signer, FixedSigner, Keyring, Receipt, ReceiptHash, ReceiptLog, ReceiptOp, ReceiptSig,
    ReserveProof, Signer, VerifyError,
};
