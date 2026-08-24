//! Payment errors mapped from the AITP-402 stable error codes (SPEC §7).

use thiserror::Error;

/// Stable AITP-402 error codes (SPEC §7) plus transport/codec failures the
/// caller side can hit before ever reaching the verifier.
#[derive(Debug, Error)]
pub enum PayError {
    #[error("insufficient_funds")]
    InsufficientFunds,
    #[error("allowance_exceeded")]
    AllowanceExceeded,
    #[error("hold_not_found")]
    HoldNotFound,
    #[error("capture_exceeds_hold")]
    CaptureExceedsHold,
    #[error("nonce_replayed")]
    NonceReplayed,
    #[error("authorization_expired")]
    AuthorizationExpired,
    #[error("signature_invalid")]
    SignatureInvalid,
    #[error("account_frozen")]
    AccountFrozen,
    #[error("idempotency_conflict")]
    IdempotencyConflict,

    /// A challenge/authorization/receipt header failed to decode or parse.
    #[error("codec error: {0}")]
    Codec(String),

    /// Any other gateway-side failure (network, unexpected status, etc.).
    #[error("gateway error: {0}")]
    Gateway(String),
}
