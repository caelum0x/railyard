//! Wire types for the AITP-402 handshake (SPEC §5), server side.
//!
//! These serialize **byte-for-byte** like the agent's
//! `aitp-client` (`apps/agent-runtime/crates/aitp-client/src/types.rs`) so a
//! future `MintPaymentGateway` can back the client's `PaymentGateway` trait:
//! identical field **declaration order**, identical `Scheme` renames, and the
//! amount as a bare `i64` (`mint_core::Micros` is `#[serde(transparent)]`, just
//! like the client's `AmountMicros`). Do NOT reorder fields — the transport
//! codec (see [`crate::codec`]) is declaration-order JSON, so order is wire.

use mint_core::Micros;
use serde::{Deserialize, Serialize};

/// Payment scheme carried in the [`Challenge`] (SPEC §5.1). Renames match the
/// agent client exactly (`aitp-hold` / `aitp-transfer`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Scheme {
    /// `aitp-hold`: reserve up to `amount_micros`, capture actual usage after
    /// the request completes. Success-only billing for agents (SPEC ADR-0002).
    #[serde(rename = "aitp-hold")]
    Hold,
    /// `aitp-transfer`: pay-now, exact price known up front.
    #[serde(rename = "aitp-transfer")]
    Transfer,
}

/// The `AITP-Challenge` a protected endpoint returns with a `402` (SPEC §5.1).
/// Field order is declaration order = wire order (keep in sync with the client).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Challenge {
    /// Provider receive address (`ait1…`).
    pub payee: String,
    /// Price for this request, in microdollars (bare `i64` on the wire).
    pub amount_micros: Micros,
    /// Always `"AIT"` (SPEC §5.1 — the closed-loop settlement unit).
    pub currency: String,
    /// Resource being paid for, e.g. `POST /v1/chat/completions#llama-3.1-70b`.
    pub resource: String,
    /// Single-use nonce (SPEC §5.4).
    pub nonce: String,
    /// Expiry, unix milliseconds.
    pub expiry: i64,
    /// Hold vs transfer.
    pub scheme: Scheme,
}

/// The signed `AITP-Authorization` the caller retries with (SPEC §5.2). Note the
/// amount field is `amount_micros` (NOT `amount`). The scheme is echoed from the
/// challenge and included in the signed intent so independently-operated 402
/// issuers (Relay/Turnstile) can settle through Mint without Mint owning their
/// process-local challenge state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Authorization {
    /// Paying account (`acct_…`).
    pub payer: String,
    /// Provider receive address, echoed from the challenge.
    pub payee: String,
    /// Amount authorized, in microdollars.
    pub amount_micros: Micros,
    /// Echoes the challenge nonce.
    pub nonce: String,
    /// Echoes the challenge resource.
    pub resource: String,
    /// Echoes the challenge expiry (unix millis).
    pub expiry: i64,
    /// Echoes the challenge settlement scheme and is covered by `op_hash`.
    pub scheme: Scheme,
    /// Idempotency key threaded into the ledger op (SPEC §5.4).
    pub idempotency_key: String,
    /// Id of the payer key that produced `sig` (resolved via `PayerKeys`).
    pub pubkey_id: String,
    /// blake3 of the canonical signed intent, lowercase hex (SPEC §3).
    pub op_hash: String,
    /// `ed25519(op_hash_bytes)`, base64url (URL-safe, no pad).
    pub sig: String,
}
