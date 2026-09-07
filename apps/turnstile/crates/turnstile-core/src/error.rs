//! Turnstile edge errors, wrapping the reused [`PayError`] plus verifier/replay
//! failures that happen at the paywall boundary before (or instead of) a mint
//! round-trip.

use aitp_client::PayError;
use thiserror::Error;

/// An error at the Turnstile edge. AITP-402 protocol/settlement failures arrive
/// as a wrapped [`PayError`]; the remaining variants are edge-only conditions
/// (bad header, mismatched challenge, replayed/expired nonce, misconfig).
#[derive(Debug, Error)]
pub enum TurnstileError {
    /// A settlement/protocol error surfaced by the reused AITP-402 layer.
    #[error(transparent)]
    Pay(#[from] PayError),

    /// The `AITP-Authorization` header could not be decoded/parsed.
    #[error("invalid authorization header: {0}")]
    InvalidAuthorization(String),

    /// The authorization does not match the resource/payee/price we challenged.
    #[error("authorization does not match challenge: {0}")]
    ChallengeMismatch(String),

    /// The nonce is unknown or was already consumed (single-use replay guard).
    #[error("nonce replayed or unknown: {0}")]
    NonceReplayed(String),

    /// The nonce (or authorization) is past its expiry.
    #[error("nonce expired")]
    NonceExpired,

    /// The durable nonce store backend (e.g. Postgres) failed. Distinct from a
    /// payment rejection: this is infrastructure, not a bad authorization.
    #[error("nonce store error: {0}")]
    Storage(String),

    /// The durable settlement journal rejected a state transition or lease.
    #[error("settlement journal error: {0}")]
    Settlement(String),

    /// A required configuration value was missing or malformed at startup.
    #[error("configuration error: {0}")]
    Config(String),
}
