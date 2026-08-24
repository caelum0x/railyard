//! Error surface for the AITP-402 protocol layer (SPEC §7).
//!
//! Settlement failures reuse the stable [`mint_core::MintError`] enum, which AP3
//! extended with the §7 protocol/allowance codes (`nonce_replayed`,
//! `authorization_expired`, `signature_invalid`, `allowance_exceeded`,
//! `account_frozen`). Keeping ONE error enum means a `Display`/`code()` string
//! surfaced by the ledger, the protocol, or a future HTTP layer is identical.
//!
//! Transport framing errors (bad base64 / malformed JSON headers) are a separate
//! concern and live in [`crate::codec::CodecError`] — they are not settlement
//! outcomes and never reach the ledger.

pub use crate::codec::CodecError;
pub use mint_core::MintError;
