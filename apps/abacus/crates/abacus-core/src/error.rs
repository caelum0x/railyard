//! [`AbacusError`] — the single domain error enum with stable snake_case `code`s
//! and HTTP status mapping.
//!
//! `status_for()` mirrors mint-server's `error.rs`:
//!
//! | code                                    | status |
//! |-----------------------------------------|--------|
//! | `allowance_exceeded`, `insufficient_funds` | 402 |
//! | `account_frozen`                        | 403    |
//! | `*_not_found` (account/hold/allowance)  | 404    |
//! | `idempotency_conflict`                  | 409    |
//! | `capture_exceeds_hold`                   | 422    |
//! | `payout_limit_exceeded`                  | 429    |

use serde::{Deserialize, Serialize};

/// Every failure mode Abacus can surface. Carries just enough context to render a
/// user-safe envelope; `Transport`/`Storage`/`Config` are mapped to a generic 5xx
/// with a scrubbed message so internal detail never leaks to clients.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "code", content = "detail", rename_all = "snake_case")]
pub enum AbacusError {
    /// The agent's budget allowance ceiling would be breached by `est_max`.
    #[error("allowance exceeded")]
    AllowanceExceeded,
    /// The funding account cannot cover the estimated hold. Short-circuits.
    #[error("insufficient funds")]
    InsufficientFunds,
    /// The funding/treasury account is frozen and cannot move funds.
    #[error("account frozen")]
    AccountFrozen,
    /// Referenced Mint account does not exist.
    #[error("account not found")]
    AccountNotFound,
    /// Referenced hold does not exist (or is already terminal).
    #[error("hold not found")]
    HoldNotFound,
    /// Referenced allowance does not exist.
    #[error("allowance not found")]
    AllowanceNotFound,
    /// Idempotency key replayed with a different body (Mint 409).
    #[error("idempotency conflict")]
    IdempotencyConflict,
    /// Capture amount exceeded the original hold (Mint 422).
    #[error("capture exceeds hold")]
    CaptureExceedsHold,
    /// Per-tx / daily payout limit would be breached.
    #[error("payout limit exceeded")]
    PayoutLimitExceeded,
    /// Missing / invalid ingress API key.
    #[error("unauthorized")]
    Unauthorized,
    /// A money amount failed its non-negative / `> 0` invariant.
    #[error("invalid amount")]
    InvalidAmount,
    /// Integer money arithmetic overflowed the `i64` domain.
    #[error("arithmetic overflow")]
    Overflow,
    /// A typed id string failed to parse at the boundary.
    #[error("bad id")]
    BadId,
    /// No pricing entry for the requested provider/model.
    #[error("model not priced")]
    ModelNotPriced,
    /// Network / transport failure talking to Mint or the upstream provider.
    #[error("transport error: {0}")]
    Transport(String),
    /// Persistence failure in the receipt store / budget repo.
    #[error("storage error: {0}")]
    Storage(String),
    /// A non-mapped upstream error from Mint.
    #[error("upstream {status}: {code}: {message}")]
    Upstream {
        /// Upstream HTTP status.
        status: u16,
        /// Upstream stable error code, if any.
        code: String,
        /// Upstream human message.
        message: String,
    },
    /// Missing / invalid startup configuration (env var).
    #[error("config error: {0}")]
    Config(String),
}

impl AbacusError {
    /// The stable snake_case code for the error envelope.
    pub fn code(&self) -> &'static str {
        match self {
            AbacusError::AllowanceExceeded => "allowance_exceeded",
            AbacusError::InsufficientFunds => "insufficient_funds",
            AbacusError::AccountFrozen => "account_frozen",
            AbacusError::AccountNotFound => "account_not_found",
            AbacusError::HoldNotFound => "hold_not_found",
            AbacusError::AllowanceNotFound => "allowance_not_found",
            AbacusError::IdempotencyConflict => "idempotency_conflict",
            AbacusError::CaptureExceedsHold => "capture_exceeds_hold",
            AbacusError::PayoutLimitExceeded => "payout_limit_exceeded",
            AbacusError::Unauthorized => "unauthorized",
            AbacusError::InvalidAmount => "invalid_amount",
            AbacusError::Overflow => "overflow",
            AbacusError::BadId => "bad_id",
            AbacusError::ModelNotPriced => "model_not_priced",
            AbacusError::Transport(_) => "transport_error",
            AbacusError::Storage(_) => "storage_error",
            AbacusError::Upstream { .. } => "upstream_error",
            AbacusError::Config(_) => "config_error",
        }
    }

    /// HTTP status this error should surface at the proxy edge. Mirrors the mint
    /// error.rs mapping so the enforcement point speaks the same status language.
    pub fn status_for(&self) -> u16 {
        match self {
            AbacusError::AllowanceExceeded | AbacusError::InsufficientFunds => 402,
            AbacusError::AccountFrozen => 403,
            AbacusError::AccountNotFound
            | AbacusError::HoldNotFound
            | AbacusError::AllowanceNotFound => 404,
            AbacusError::IdempotencyConflict => 409,
            AbacusError::CaptureExceedsHold => 422,
            AbacusError::PayoutLimitExceeded => 429,
            AbacusError::Unauthorized => 401,
            AbacusError::InvalidAmount
            | AbacusError::Overflow
            | AbacusError::BadId
            | AbacusError::ModelNotPriced => 400,
            AbacusError::Upstream { status, .. } => *status,
            AbacusError::Transport(_) | AbacusError::Storage(_) | AbacusError::Config(_) => 502,
        }
    }

    /// A client-safe message: never leaks storage/transport/config internals.
    pub fn client_message(&self) -> String {
        match self {
            AbacusError::Transport(_) => "upstream unavailable".to_string(),
            AbacusError::Storage(_) => "internal error".to_string(),
            AbacusError::Config(_) => "internal error".to_string(),
            other => other.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping_mirrors_mint() {
        assert_eq!(AbacusError::AllowanceExceeded.status_for(), 402);
        assert_eq!(AbacusError::InsufficientFunds.status_for(), 402);
        assert_eq!(AbacusError::AccountFrozen.status_for(), 403);
        assert_eq!(AbacusError::AccountNotFound.status_for(), 404);
        assert_eq!(AbacusError::HoldNotFound.status_for(), 404);
        assert_eq!(AbacusError::AllowanceNotFound.status_for(), 404);
        assert_eq!(AbacusError::IdempotencyConflict.status_for(), 409);
        assert_eq!(AbacusError::CaptureExceedsHold.status_for(), 422);
        assert_eq!(AbacusError::PayoutLimitExceeded.status_for(), 429);
    }

    #[test]
    fn transport_message_is_scrubbed() {
        let e = AbacusError::Transport("connection refused to 10.0.0.1".into());
        assert_eq!(e.client_message(), "upstream unavailable");
        assert_eq!(e.status_for(), 502);
    }

    #[test]
    fn codes_are_stable_snake_case() {
        assert_eq!(AbacusError::AllowanceExceeded.code(), "allowance_exceeded");
        assert_eq!(AbacusError::CaptureExceedsHold.code(), "capture_exceeds_hold");
        assert_eq!(AbacusError::PayoutLimitExceeded.code(), "payout_limit_exceeded");
    }
}
