//! `mint-core` — pure, dependency-light value types + money math shared by the
//! whole Mint stack. NO I/O, NO async, NO store.
//!
//! - [`micros`]  — [`Micros`] microdollar newtype with checked integer math and
//!   the validated [`TransferAmount`] wrapper.
//! - [`ids`]     — [`AccountId`] / [`AccountKind`] identity types.
//! - [`address`] — bech32 `ait1…` [`Address`] encode/decode + checksum.
//! - [`hash`]    — canonical-JSON → blake3 [`OpHash`] helper ([`op_hash`]).
//! - [`error`]   — the stable [`MintError`] enum (codes mirror AITP-402 §7).

pub mod address;
pub mod error;
pub mod hash;
pub mod ids;
pub mod micros;

pub use address::Address;
pub use error::MintError;
pub use hash::{op_hash, seed_bytes16, OpHash};
pub use ids::{AccountId, AccountKind, AllowanceId, HoldId, StreamId};
pub use micros::{Micros, TransferAmount, MICROS_PER_AIT};
