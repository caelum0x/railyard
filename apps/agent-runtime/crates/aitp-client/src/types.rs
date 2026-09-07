//! Wire types mirroring the AITP-402 handshake (SPEC §5), caller side only.
//!
//! Server-side verify/settle lives in Mint; this crate models only what an
//! agent needs to answer a `402 Payment Required`.

use serde::{Deserialize, Serialize};

use crate::amount::AmountMicros;

/// Payment scheme carried in the challenge (SPEC §5.1).
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

/// Decoded `AITP-Challenge` header (server → caller, SPEC §5.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentChallenge {
    /// Provider receive address (`ait1…`).
    pub payee: String,
    /// Price for this request, in microdollars.
    pub amount_micros: AmountMicros,
    /// Always `"AIT"`.
    pub currency: String,
    /// Resource being paid for, e.g. `POST /v1/chat/completions#llama-3.1-70b`.
    pub resource: String,
    /// Single-use nonce.
    pub nonce: String,
    /// Expiry, unix milliseconds.
    pub expiry: i64,
    /// Hold vs transfer.
    pub scheme: Scheme,
}

/// Signed `AITP-Authorization` header (caller → server, SPEC §5.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaymentAuthorization {
    /// Paying account (`acct_…`).
    pub payer: String,
    /// Provider receive address, echoed from the challenge.
    pub payee: String,
    pub amount_micros: AmountMicros,
    /// Echoes the challenge nonce.
    pub nonce: String,
    pub resource: String,
    pub expiry: i64,
    /// Echoes the challenged settlement scheme and is covered by `op_hash`.
    pub scheme: Scheme,
    pub idempotency_key: String,
    pub pubkey_id: String,
    /// blake3 of the canonical intent (hex/base64url). Fake in the stub.
    pub op_hash: String,
    /// ed25519(op_hash), base64url. Fake in the stub.
    pub sig: String,
}

/// Parsed `AITP-Receipt` header (server → caller on settle, SPEC §6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Receipt {
    #[serde(alias = "sequence")]
    pub seq: u64,
    #[serde(alias = "operation")]
    pub op: String,
    #[serde(alias = "operation_hash")]
    pub op_hash: String,
    pub amount_micros: AmountMicros,
    #[serde(default)]
    pub balance_after_micros: AmountMicros,
    #[serde(default)]
    pub prev_hash: String,
    #[serde(alias = "receipt_hash")]
    pub hash: String,
    #[serde(alias = "signature")]
    pub sig: String,
}

/// Opaque reference to a reservation created by an `aitp-hold` authorization.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldRef(pub String);
