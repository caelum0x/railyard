//! Turnstile core — a framework-agnostic AITP-402 paywall engine.
//!
//! Turnstile is the SERVER/VERIFIER side of the AITP-402 handshake. It reuses
//! the REAL [`aitp_client`] crate (path dep on `apps/agent-runtime`) for the wire
//! contract — [`PaymentChallenge`], [`PaymentAuthorization`], [`Receipt`],
//! [`HoldRef`], [`Scheme`], the header [`codec`](aitp_client::codec), the
//! injectable [`HttpTransport`](aitp_client::transport::HttpTransport) seam, and
//! [`PayError`] — and never re-implements crypto or the codec.
//!
//! Key boundary: `aitp_client` models the CALLER side; its `MintPaymentGateway`
//! re-signs authorizations, so Turnstile (the verifier) canNOT reuse it. Instead
//! Turnstile FORWARDS the agent's already-signed authorization to the live
//! mint-server via [`verifier::MintVerifier`], then captures the actual amount on
//! success or releases the hold on failure (success-only billing, SPEC ADR-0002).
//!
//! Pipeline: [`guard::Turnstile::admit`] → issue a [`Challenge402`](guard::Admit)
//! when no authorization is present, else validate + replay-check the nonce
//! ([`nonce`]), forward to the mint ([`verifier`]), and hand back a
//! [`settle::Settled`] hold the edge captures/releases.

pub mod challenge;
pub mod clock;
pub mod config;
pub mod error;
pub mod guard;
pub mod nonce;
pub mod settle;
pub mod verifier;

// Re-export the whole reused crate plus the wire types callers touch directly,
// so integrations depend on `turnstile_core::*` without a second import.
pub use aitp_client;
pub use aitp_client::{
    AmountMicros, HoldRef, PayError, PaymentAuthorization, PaymentChallenge, Receipt, Scheme,
    MICROS_PER_AIT,
};

pub use challenge::ChallengeIssuer;
pub use config::TurnstileConfig;
pub use error::TurnstileError;
pub use guard::{Admit, Turnstile};
pub use nonce::{InMemoryNonceStore, NonceStore};
pub use settle::Settled;
pub use verifier::{FakeVerifier, MintVerifier, PaymentVerifier, VerifiedHold};
