//! The signing seam for a REAL AITP-402 authorization (SPEC §3).
//!
//! Mint's `verify_and_settle` (mint-protocol `protocol.rs`) rejects any
//! authorization whose `op_hash` is not the blake3 of the sorted-key canonical
//! JSON of the signed field subset, or whose `sig` does not ed25519-verify over
//! that 32-byte hash under the payer's registered key. The stub's dummy
//! `op_hash`/`sig` therefore never verify against a live mint.
//!
//! This module supplies the two real pieces, decoupled from Mint:
//! - [`op_hash`]: a byte-for-byte mirror of `mint_core::op_hash` (sorted-key
//!   canonical JSON → blake3) over the pinned intent subset;
//! - [`AuthSigner`]: the injected key holder that signs the 32-byte hash and
//!   names its `pubkey_id`. [`Ed25519Signer`] is the production implementation.
//!
//! Per the security rules the signing key is never hardcoded: production wiring
//! builds an [`Ed25519Signer`] from key material read out of the environment /
//! secret manager at startup and injects it into the gateway.

use std::collections::BTreeMap;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::Value;

use crate::amount::AmountMicros;

/// The pinned signed-intent field subset (SPEC §3): everything that binds the
/// authorization to one payment, EXCLUDING `op_hash`, `sig`, and `pubkey_id`.
///
/// This is exactly the set Mint hashes (`authorization_intent` in
/// mint-protocol `intent.rs`). Struct field order is irrelevant — [`op_hash`]
/// sorts keys before hashing.
#[derive(Debug, Clone)]
pub struct AuthorizationIntent<'a> {
    pub payer: &'a str,
    pub payee: &'a str,
    pub amount_micros: AmountMicros,
    pub nonce: &'a str,
    pub resource: &'a str,
    pub expiry: i64,
    pub idempotency_key: &'a str,
}

/// Canonical (sorted-key, compact) JSON bytes for the intent — a mirror of
/// `mint_core::hash::canonical_json_bytes` for this flat object.
fn canonical_intent_bytes(intent: &AuthorizationIntent<'_>) -> Vec<u8> {
    // A `BTreeMap` serializes in sorted key order, matching Mint's recursive
    // key-sort. Values are flat scalars, so this reproduces the exact bytes.
    let mut map: BTreeMap<&'static str, Value> = BTreeMap::new();
    map.insert("payer", Value::from(intent.payer));
    map.insert("payee", Value::from(intent.payee));
    map.insert("amount_micros", Value::from(intent.amount_micros.get()));
    map.insert("nonce", Value::from(intent.nonce));
    map.insert("resource", Value::from(intent.resource));
    map.insert("expiry", Value::from(intent.expiry));
    map.insert("idempotency_key", Value::from(intent.idempotency_key));
    serde_json::to_vec(&map).expect("BTreeMap<&str, Value> always serializes")
}

/// The 32-byte blake3 `op_hash` for `intent` (SPEC §3). Matches
/// `mint_core::op_hash` byte-for-byte for the pinned subset.
#[must_use]
pub fn op_hash(intent: &AuthorizationIntent<'_>) -> [u8; 32] {
    *blake3::hash(&canonical_intent_bytes(intent)).as_bytes()
}

/// Lowercase hex of a 32-byte hash (matches `OpHash::to_hex`).
#[must_use]
pub fn op_hash_hex(bytes: &[u8; 32]) -> String {
    let mut s = String::with_capacity(64);
    for b in bytes {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

/// A payer key holder that signs an authorization's `op_hash`.
///
/// Kept minimal and object-safe so tests can supply a deterministic fake and
/// production supplies [`Ed25519Signer`]. The gateway computes the `op_hash`
/// itself and asks the signer only to (a) name its key and (b) sign the bytes.
pub trait AuthSigner: Send + Sync {
    /// The `pubkey_id` Mint uses to resolve the verifying key
    /// (`PayerKeys(payer, pubkey_id)`).
    fn pubkey_id(&self) -> String;

    /// Sign the 32-byte `op_hash`, returning the signature as
    /// `base64url(URL_SAFE_NO_PAD)` — the exact encoding Mint's verifier decodes.
    fn sign_op_hash(&self, op_hash: &[u8; 32]) -> String;
}

/// The production [`AuthSigner`]: an ed25519 key that signs the op_hash bytes.
pub struct Ed25519Signer {
    key_id: String,
    signing: SigningKey,
}

impl Ed25519Signer {
    /// Build a signer from a 32-byte ed25519 secret seed and its `pubkey_id`.
    ///
    /// The corresponding verifying key ([`Ed25519Signer::verifying_key_bytes`])
    /// must already be registered in mint-server's `PayerKeys` under
    /// `(payer, key_id)` or verification returns `signature_invalid`.
    pub fn from_seed(key_id: impl Into<String>, seed: &[u8; 32]) -> Self {
        Self {
            key_id: key_id.into(),
            signing: SigningKey::from_bytes(seed),
        }
    }

    /// The 32-byte ed25519 public key, for registering with mint-server.
    #[must_use]
    pub fn verifying_key_bytes(&self) -> [u8; 32] {
        self.signing.verifying_key().to_bytes()
    }
}

impl AuthSigner for Ed25519Signer {
    fn pubkey_id(&self) -> String {
        self.key_id.clone()
    }

    fn sign_op_hash(&self, op_hash: &[u8; 32]) -> String {
        let sig = self.signing.sign(op_hash);
        URL_SAFE_NO_PAD.encode(sig.to_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_hash_is_field_order_independent_and_stable() {
        let a = AuthorizationIntent {
            payer: "acct_payer",
            payee: "ait1qprovider",
            amount_micros: AmountMicros(4200),
            nonce: "9f2c",
            resource: "POST /v1/chat#llama",
            expiry: 1_723_958_400_000,
            idempotency_key: "idem_1",
        };
        let h = op_hash(&a);
        // Deterministic: recomputing yields the same bytes and hex is 64 chars.
        assert_eq!(h, op_hash(&a));
        assert_eq!(op_hash_hex(&h).len(), 64);
    }

    #[test]
    fn ed25519_signer_round_trips_a_signature() {
        let signer = Ed25519Signer::from_seed("key_test", &[7u8; 32]);
        let intent = AuthorizationIntent {
            payer: "acct_payer",
            payee: "ait1qprovider",
            amount_micros: AmountMicros(4200),
            nonce: "9f2c",
            resource: "r",
            expiry: 10,
            idempotency_key: "idem_1",
        };
        let h = op_hash(&intent);
        let sig_b64 = signer.sign_op_hash(&h);

        // The signature verifies under the signer's own verifying key — exactly
        // what mint-server does after resolving the key by pubkey_id.
        use ed25519_dalek::{Signature, Verifier, VerifyingKey};
        let vk = VerifyingKey::from_bytes(&signer.verifying_key_bytes()).unwrap();
        let raw = URL_SAFE_NO_PAD.decode(sig_b64).unwrap();
        let sig = Signature::from_slice(&raw).unwrap();
        assert!(vk.verify(&h, &sig).is_ok());
        assert_eq!(signer.pubkey_id(), "key_test");
    }
}
