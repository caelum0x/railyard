//! `mint-receipts` — signed, hash-chained receipt log for the Mint settlement
//! layer (AITP-402 §6).
//!
//! Every applied ledger op appends one [`Receipt`] per touched account to a
//! per-account BLAKE3 hash chain, Ed25519-signed by a mint-verifier key. The
//! full log can be:
//! - **verified offline** with a public-key-only [`Keyring`] ([`verify_log`]);
//! - summarized into a Merkle audit root ([`merkle_root`]);
//! - turned into a proof-of-reserves ([`reserve_proof_from_receipts`]).
//!
//! This crate has **no dependency on `mint-store`**: the store maps its own
//! `OpKind` onto [`ReceiptOp`] at the call site, keeping the graph acyclic
//! (`mint-core → mint-receipts → mint-store`).
//!
//! - [`receipt`]  — [`ReceiptHash`], [`ReceiptSig`], [`SignerKeyId`],
//!   [`ReceiptOp`], [`ReceiptBody`], [`Receipt`].
//! - [`signer`]   — [`Signer`] trait, [`Ed25519Signer`], [`FixedSigner`],
//!   [`Keyring`].
//! - [`log`]      — [`ReceiptDraft`], [`ReceiptLog`].
//! - [`verify`]   — [`VerifyError`], [`verify_chain`], [`verify_log`].
//! - [`merkle`]   — [`merkle_root`], [`verify_merkle`], [`MerkleProof`].
//! - [`reserve`]  — [`ReserveProof`], [`reserve_proof`],
//!   [`reserve_proof_from_receipts`].

pub mod log;
pub mod merkle;
pub mod receipt;
pub mod reserve;
pub mod signer;
pub mod verify;

pub use log::{ReceiptDraft, ReceiptLog};
pub use merkle::{merkle_root, prove, verify_merkle, MerkleProof};
pub use receipt::{Receipt, ReceiptBody, ReceiptHash, ReceiptOp, ReceiptSig, SignerKeyId};
pub use reserve::{reserve_proof, reserve_proof_from_receipts, ReserveProof};
pub use signer::{Ed25519Signer, FixedSigner, Keyring, Signer, DEFAULT_KEY_ID, FIXED_SEED};
pub use verify::{verify_chain, verify_log, VerifyError};
