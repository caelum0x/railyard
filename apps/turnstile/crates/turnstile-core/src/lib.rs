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
//!
//! The [`x402`] module adds the open x402 standard (USDC `exact` payments
//! verified and settled through a facilitator, credited into Mint) as a second
//! rail on the same gate; AITP-402 keeps working unchanged.

pub mod challenge;
pub mod clock;
pub mod config;
pub mod error;
pub mod guard;
pub mod journal;
pub mod mcp;
pub mod nonce;
#[cfg(feature = "postgres")]
pub mod pg;
#[cfg(feature = "postgres")]
pub mod pg_settlement;
#[cfg(feature = "postgres")]
pub mod pg_tools;
pub mod recovery;
pub mod registry;
pub mod settle;
pub mod tool_store;
pub mod verifier;
pub mod x402;

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
pub use journal::{
    InMemorySettlementJournal, ResourceRevenue, RevenueSummary, SettlementJournal,
    SettlementPhase, SettlementRecord,
};
pub use mcp::{McpAdmit, McpPaywall, McpX402Refusal};
pub use nonce::{InMemoryNonceStore, NonceStore};
#[cfg(feature = "postgres")]
pub use pg::{spawn_reaper, PostgresNonceStore};
#[cfg(feature = "postgres")]
pub use pg_settlement::PostgresSettlementJournal;
#[cfg(feature = "postgres")]
pub use pg_tools::PostgresToolStore;
pub use recovery::{reconcile_once, spawn_settlement_reaper, RecoveryReport};
pub use registry::{ToolCatalog, ToolDescriptor, ToolRegistry};
pub use settle::Settled;
pub use tool_store::{
    InMemoryToolStore, StoredTool, ToolAction, ToolAuditEntry, ToolStore, ToolStoreError,
};
pub use verifier::{FakeVerifier, MintVerifier, PaymentVerifier, VerifiedHold};
pub use x402::{X402Config, X402Gate};
