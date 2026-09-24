//! [`VendError`] — the single domain error enum, with stable string `code`s and
//! HTTP status mapping consumed by the `{success,error:{code,message}}` envelope.

use serde::{Deserialize, Serialize};

/// Every failure mode Vend can surface. Carries just enough context to render a
/// user-safe envelope; never leaks internal storage/transport detail verbatim to
/// clients (the server maps `Storage`/`Transport` to a generic 5xx message).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "code", content = "detail", rename_all = "snake_case")]
pub enum VendError {
    /// End-user / tenant reserve cannot cover the estimated hold. Short-circuits.
    #[error("insufficient funds")]
    InsufficientFunds,
    /// Mint reported the reserve itself is short. Short-circuits.
    #[error("reserve insufficient")]
    ReserveInsufficient,
    /// Idempotency key was reused with a different request body (Mint 409).
    #[error("idempotency conflict")]
    IdempotencyConflict,
    /// Metered requests require a bounded, printable client idempotency key.
    #[error("Idempotency-Key header is required")]
    IdempotencyRequired,
    /// Another request with the same idempotency key is still executing.
    #[error("request with this Idempotency-Key is still in progress")]
    IdempotencyInProgress,
    /// A streaming key is deliberately single-use because emitted bytes cannot
    /// be atomically replayed after a partial delivery or disconnect.
    #[error("this streaming Idempotency-Key has already been consumed")]
    StreamingIdempotencyConsumed,
    /// The configured Relay transport does not implement live streaming.
    #[error("streaming is not supported by this Vend deployment")]
    StreamingUnsupported,
    /// Sx0 supports only the tenant-reserve settlement path.
    #[error("funding mode is not supported by this Vend deployment")]
    FundingModeUnsupported,
    /// Capture amount exceeded the original hold (Mint 422).
    #[error("capture exceeds hold")]
    CaptureExceedsHold,
    /// Per-key or per-end-user daily spend cap would be breached.
    #[error("spend cap exceeded")]
    SpendCapExceeded,
    /// Missing/invalid provisioning key, or a revoked key.
    #[error("unauthorized")]
    Unauthorized,
    /// Referenced tenant/app/end-user/key row does not exist.
    #[error("not found")]
    NotFound,
    /// A money amount failed its `> 0` (or non-negative) invariant.
    #[error("invalid amount")]
    InvalidAmount,
    /// Integer money arithmetic overflowed the `i64` domain.
    #[error("arithmetic overflow")]
    Overflow,
    /// A typed ULID id string failed to parse.
    #[error("bad id")]
    BadId,
    /// Requested model is not in the app's allowlist.
    #[error("model not allowed")]
    ModelNotAllowed,
    /// Network / transport failure talking to Mint or Relay.
    #[error("transport error: {0}")]
    Transport(String),
    /// Persistence failure in vend-store.
    #[error("storage error: {0}")]
    Storage(String),
    /// A non-mapped upstream error from Mint or Relay.
    #[error("upstream {status}: {code}: {message}")]
    Upstream {
        /// Upstream HTTP status.
        status: u16,
        /// Upstream stable error code, if any.
        code: String,
        /// Upstream human message.
        message: String,
    },
    /// Missing/invalid startup configuration (env var).
    #[error("config error: {0}")]
    Config(String),
    /// The end user exists for attribution only and has no Mint wallet, so it
    /// cannot fund `end_user_reserve` requests or receive top-ups.
    #[error("end user has no wallet; it was provisioned for attribution only")]
    WalletUnprovisioned,
    /// An optional product surface is switched off by deployment config. The
    /// message names the setting to change and carries no secret.
    #[error("{0}")]
    FeatureDisabled(String),
}

impl VendError {
    /// Stable snake_case code for the error envelope.
    pub fn code(&self) -> &'static str {
        match self {
            VendError::InsufficientFunds => "insufficient_funds",
            VendError::ReserveInsufficient => "reserve_insufficient",
            VendError::IdempotencyConflict => "idempotency_conflict",
            VendError::IdempotencyRequired => "idempotency_key_required",
            VendError::IdempotencyInProgress => "idempotency_in_progress",
            VendError::StreamingIdempotencyConsumed => "streaming_idempotency_consumed",
            VendError::StreamingUnsupported => "streaming_unsupported",
            VendError::FundingModeUnsupported => "funding_mode_unsupported",
            VendError::CaptureExceedsHold => "capture_exceeds_hold",
            VendError::SpendCapExceeded => "spend_cap_exceeded",
            VendError::Unauthorized => "unauthorized",
            VendError::NotFound => "not_found",
            VendError::InvalidAmount => "invalid_amount",
            VendError::Overflow => "overflow",
            VendError::BadId => "bad_id",
            VendError::ModelNotAllowed => "model_not_allowed",
            VendError::Transport(_) => "transport_error",
            VendError::Storage(_) => "storage_error",
            VendError::Upstream { .. } => "upstream_error",
            VendError::Config(_) => "config_error",
            VendError::WalletUnprovisioned => "wallet_unprovisioned",
            VendError::FeatureDisabled(_) => "feature_disabled",
        }
    }

    /// HTTP status the public data plane should return for this error.
    pub fn http_status(&self) -> u16 {
        match self {
            VendError::InsufficientFunds | VendError::ReserveInsufficient => 402,
            VendError::SpendCapExceeded => 402,
            VendError::IdempotencyConflict
            | VendError::IdempotencyInProgress
            | VendError::StreamingIdempotencyConsumed => 409,
            VendError::IdempotencyRequired => 400,
            VendError::StreamingUnsupported | VendError::FundingModeUnsupported => 400,
            VendError::CaptureExceedsHold => 422,
            VendError::Unauthorized => 401,
            VendError::NotFound => 404,
            VendError::InvalidAmount | VendError::BadId | VendError::ModelNotAllowed => 400,
            VendError::Overflow => 400,
            VendError::Upstream { status, .. } => *status,
            VendError::Transport(_) | VendError::Storage(_) | VendError::Config(_) => 502,
            VendError::WalletUnprovisioned => 409,
            VendError::FeatureDisabled(_) => 503,
        }
    }

    /// A client-safe message: never leaks storage/transport internals.
    pub fn client_message(&self) -> String {
        match self {
            VendError::Transport(_) => "upstream unavailable".to_string(),
            VendError::Storage(_) => "internal error".to_string(),
            VendError::Config(_) => "internal error".to_string(),
            other => other.to_string(),
        }
    }
}
