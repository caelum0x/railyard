//! `mint-protocol` — the AITP-402 payment-required protocol layer that binds the
//! Mint crate graph ([`mint_core`], [`mint_store`], [`mint_receipts`],
//! [`mint_ledger`]) into a wire-compatible challenge/authorization/settlement
//! state machine.
//!
//! The module surface layers the concern cleanly:
//!
//! - [`types`]    — wire structs (`Challenge`, `Authorization`, `Scheme`) that
//!   serialize byte-for-byte like the AITP client, reusing
//!   [`mint_core::Micros`] as the amount.
//! - [`codec`]    — TRANSPORT layer: URL-safe-base64 over compact
//!   (declaration-order) JSON for the header envelope.
//! - [`intent`]   — SIGNING layer: sorted-key canonical JSON → blake3 `op_hash`
//!   and ed25519 authorization construction.
//! - [`nonce`]    — replay-protection nonce store seam.
//! - [`verifier`] — per-payer verifying-key seam ([`PayerKeys`]).
//! - [`protocol`] — the `Aitp402` challenge/verify/settle state machine.
//! - [`error`]    — AITP-402 §7 error surface over [`mint_core::MintError`].

pub mod codec;
pub mod error;
pub mod intent;
pub mod nonce;
pub mod protocol;
pub mod types;
pub mod verifier;

pub use codec::{
    decode_authorization, decode_challenge, decode_receipt, encode_authorization, encode_challenge,
    encode_receipt, AitpReceipt, CodecError, AITP_VERSION, HEADER_AUTHORIZATION, HEADER_CHALLENGE,
    HEADER_RECEIPT, HEADER_VERSION,
};
pub use intent::{authorization_intent, build_authorization, compute_op_hash};
pub use nonce::{InMemoryNonceStore, NonceStore};
pub use protocol::{Aitp402, Settlement};
pub use types::{Authorization, Challenge, Scheme};
pub use verifier::{InMemoryPayerKeys, PayerKeys};

// Re-export the shared vocabulary so downstream callers (and tests) can drive
// the protocol without depending on the inner crates directly.
pub use mint_core::{Micros, MintError};
