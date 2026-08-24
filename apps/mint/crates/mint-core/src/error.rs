//! Stable error model. Codes mirror the transfer subset of AITP-402 §7.
//!
//! `Display` renders the *stable string code* (e.g. `insufficient_funds`) so
//! HTTP/RPC layers can surface it verbatim. `code()` returns the same `&'static
//! str` without allocating.

use thiserror::Error;

/// The stable Mint error enum. AP0 uses the transfer subset; later phases add
/// hold/stream/receipt/reserve codes without renumbering these.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MintError {
    /// A debit would drive an account's available balance negative.
    #[error("insufficient_funds")]
    InsufficientFunds,
    /// Same `idem_key` replayed with a *different* body.
    #[error("idempotency_conflict")]
    IdempotencyConflict,
    /// Checked integer arithmetic would overflow `i64`.
    #[error("overflow")]
    Overflow,
    /// A bech32 address failed to decode or its checksum was invalid.
    #[error("bad_address")]
    BadAddress,
    /// Referenced account does not exist.
    #[error("account_not_found")]
    AccountNotFound,
    /// A transfer amount was not strictly positive.
    #[error("invalid_amount")]
    InvalidAmount,
    /// An `AccountId` string was malformed (bad prefix/kind/ULID) or a
    /// transfer named the same account on both sides.
    #[error("bad_account_id")]
    BadAccountId,
    /// Referenced hold does not exist, or a distinct (non-replay) close was
    /// attempted against an already-terminal (Captured/Released) hold.
    #[error("hold_not_found")]
    HoldNotFound,
    /// A `hold.capture` requested more than the hold reserved (`capture > held`).
    #[error("capture_exceeds_hold")]
    CaptureExceedsHold,
    /// A canonical receipt rebuild found a journal group that mixes rows from
    /// distinct ops (a bad delimiter), so it refuses to emit rather than
    /// silently drop or merge receipts.
    #[error("receipt_rebuild_mismatch")]
    ReceiptRebuildMismatch,
    // ---- AITP-402 §7 protocol/allowance codes (AP3) ------------------------
    /// A challenge nonce was presented more than once inside its validity
    /// window (single-use replay protection, SPEC §5.4).
    #[error("nonce_replayed")]
    NonceReplayed,
    /// A signed authorization was presented at/after its `expiry` (SPEC §5.3).
    #[error("authorization_expired")]
    AuthorizationExpired,
    /// The authorization's `op_hash`/`sig` did not verify against the payer's
    /// registered key (bad signature, tampered body, or wrong key).
    #[error("signature_invalid")]
    SignatureInvalid,
    /// A delegated `allowance.spend` exceeded its remaining budget, violated its
    /// scope, or targeted a revoked allowance (SPEC §4).
    #[error("allowance_exceeded")]
    AllowanceExceeded,
    /// The referenced account is frozen and cannot move funds (SPEC §7).
    #[error("account_frozen")]
    AccountFrozen,
    /// A referenced allowance does not exist.
    #[error("allowance_not_found")]
    AllowanceNotFound,
    // ---- AITP-402 §4/§7 stream + reserve codes (AP4) -----------------------
    /// A referenced stream does not exist.
    #[error("stream_not_found")]
    StreamNotFound,
    /// A `stream.tick`/`stream.close` targeted an already-closed (terminal)
    /// stream.
    #[error("stream_closed")]
    StreamClosed,
    /// A `stream.tick` presented a `seq` that did not strictly exceed the
    /// stream's `last_seq` (out-of-order / duplicate tick).
    #[error("stream_seq_regressed")]
    StreamSeqRegressed,
    /// A `stream.tick`/`stream.close` would settle more than the stream's
    /// remaining cap (`settled + units*rate > cap`).
    #[error("stream_cap_exceeded")]
    StreamCapExceeded,
    /// A `redeem` would drive the in-store reserve negative (`reserve < amount`).
    #[error("reserve_insufficient")]
    ReserveInsufficient,
    /// A `redeem` was attempted below the required KYC tier.
    #[error("kyc_required")]
    KycRequired,
    /// A `redeem` exceeded the payer's per-tx / daily payout limit.
    #[error("payout_limit")]
    PayoutLimitExceeded,
    /// A `mint`'s referenced deposit was absent or not confirmed by the ramp.
    #[error("deposit_not_confirmed")]
    DepositNotConfirmed,
    /// A `mint` requested more AIT than the confirmed deposit's amount.
    #[error("deposit_amount_mismatch")]
    DepositAmountMismatch,
}

impl MintError {
    /// The stable machine-readable code string (identical to `Display`).
    pub const fn code(&self) -> &'static str {
        match self {
            MintError::InsufficientFunds => "insufficient_funds",
            MintError::IdempotencyConflict => "idempotency_conflict",
            MintError::Overflow => "overflow",
            MintError::BadAddress => "bad_address",
            MintError::AccountNotFound => "account_not_found",
            MintError::InvalidAmount => "invalid_amount",
            MintError::BadAccountId => "bad_account_id",
            MintError::HoldNotFound => "hold_not_found",
            MintError::CaptureExceedsHold => "capture_exceeds_hold",
            MintError::ReceiptRebuildMismatch => "receipt_rebuild_mismatch",
            MintError::NonceReplayed => "nonce_replayed",
            MintError::AuthorizationExpired => "authorization_expired",
            MintError::SignatureInvalid => "signature_invalid",
            MintError::AllowanceExceeded => "allowance_exceeded",
            MintError::AccountFrozen => "account_frozen",
            MintError::AllowanceNotFound => "allowance_not_found",
            MintError::StreamNotFound => "stream_not_found",
            MintError::StreamClosed => "stream_closed",
            MintError::StreamSeqRegressed => "stream_seq_regressed",
            MintError::StreamCapExceeded => "stream_cap_exceeded",
            MintError::ReserveInsufficient => "reserve_insufficient",
            MintError::KycRequired => "kyc_required",
            MintError::PayoutLimitExceeded => "payout_limit",
            MintError::DepositNotConfirmed => "deposit_not_confirmed",
            MintError::DepositAmountMismatch => "deposit_amount_mismatch",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_and_match_display() {
        let cases = [
            (MintError::InsufficientFunds, "insufficient_funds"),
            (MintError::IdempotencyConflict, "idempotency_conflict"),
            (MintError::Overflow, "overflow"),
            (MintError::BadAddress, "bad_address"),
            (MintError::AccountNotFound, "account_not_found"),
            (MintError::InvalidAmount, "invalid_amount"),
            (MintError::BadAccountId, "bad_account_id"),
            (MintError::HoldNotFound, "hold_not_found"),
            (MintError::CaptureExceedsHold, "capture_exceeds_hold"),
            (
                MintError::ReceiptRebuildMismatch,
                "receipt_rebuild_mismatch",
            ),
            (MintError::NonceReplayed, "nonce_replayed"),
            (MintError::AuthorizationExpired, "authorization_expired"),
            (MintError::SignatureInvalid, "signature_invalid"),
            (MintError::AllowanceExceeded, "allowance_exceeded"),
            (MintError::AccountFrozen, "account_frozen"),
            (MintError::AllowanceNotFound, "allowance_not_found"),
            (MintError::StreamNotFound, "stream_not_found"),
            (MintError::StreamClosed, "stream_closed"),
            (MintError::StreamSeqRegressed, "stream_seq_regressed"),
            (MintError::StreamCapExceeded, "stream_cap_exceeded"),
            (MintError::ReserveInsufficient, "reserve_insufficient"),
            (MintError::KycRequired, "kyc_required"),
            (MintError::PayoutLimitExceeded, "payout_limit"),
            (MintError::DepositNotConfirmed, "deposit_not_confirmed"),
            (MintError::DepositAmountMismatch, "deposit_amount_mismatch"),
        ];
        for (err, code) in cases {
            assert_eq!(err.code(), code);
            assert_eq!(err.to_string(), code);
        }
    }
}
