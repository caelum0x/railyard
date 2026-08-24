//! SIGNING layer for the AITP-402 authorization (SPEC §3) — the canonical
//! `op_hash` and the payer-side authorization builder.
//!
//! This is the OTHER encoding, deliberately DISTINCT from the transport codec
//! ([`crate::codec`]): the signed intent is **sorted-key canonical JSON →
//! blake3** (via [`mint_core::op_hash`]), so it is field-order independent and
//! never base64. The signature is `ed25519(op_hash_bytes)` — the same 32-byte
//! message [`mint_receipts::Signer::sign`] expects.
//!
//! This module also PINS the signed-field subset the agent client leaves as a
//! fake stub: `{payer, payee, amount_micros, nonce, resource, expiry,
//! idempotency_key}`, EXCLUDING `op_hash`, `sig`, and `pubkey_id`. Once the
//! agent signs for real, both sides hash these identical fields (SPEC §3/§5).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use mint_core::{op_hash as core_op_hash, OpHash};
use mint_receipts::Signer;
use serde_json::{json, Value};

use crate::types::{Authorization, Challenge};

/// Build the canonical signed-intent JSON from the pinned field subset. The
/// value is fed to [`mint_core::op_hash`], which sorts keys — so the order here
/// is irrelevant to the resulting hash (it is written in wire order for
/// readability only).
fn intent_value(
    payer: &str,
    payee: &str,
    amount_micros: i64,
    nonce: &str,
    resource: &str,
    expiry: i64,
    idempotency_key: &str,
) -> Value {
    json!({
        "payer": payer,
        "payee": payee,
        "amount_micros": amount_micros,
        "nonce": nonce,
        "resource": resource,
        "expiry": expiry,
        "idempotency_key": idempotency_key,
    })
}

/// The canonical signed-intent value for an [`Authorization`] (excludes
/// `op_hash`, `sig`, `pubkey_id`).
pub fn authorization_intent(a: &Authorization) -> Value {
    intent_value(
        &a.payer,
        &a.payee,
        a.amount_micros.get(),
        &a.nonce,
        &a.resource,
        a.expiry,
        &a.idempotency_key,
    )
}

/// Recompute the `op_hash` an [`Authorization`] should carry, from its signed
/// fields. Field-order independent (sorted canonical JSON → blake3).
pub fn compute_op_hash(a: &Authorization) -> OpHash {
    core_op_hash(&authorization_intent(a))
}

/// Build a fully-signed [`Authorization`] answering `challenge`, signed by the
/// payer's `signer`. This is the caller/test seam: it computes the canonical
/// `op_hash`, signs its 32 bytes, base64url-encodes the signature, and stamps
/// `pubkey_id` from the signer's key id. The produced authorization verifies
/// against a [`crate::verifier::PayerKeys`] holding the signer's public key.
pub fn build_authorization(
    payer: &str,
    challenge: &Challenge,
    idempotency_key: &str,
    signer: &dyn Signer,
) -> Authorization {
    let intent = intent_value(
        payer,
        &challenge.payee,
        challenge.amount_micros.get(),
        &challenge.nonce,
        &challenge.resource,
        challenge.expiry,
        idempotency_key,
    );
    let hash = core_op_hash(&intent);
    let sig = signer.sign(hash.as_bytes());
    Authorization {
        payer: payer.to_string(),
        payee: challenge.payee.clone(),
        amount_micros: challenge.amount_micros,
        nonce: challenge.nonce.clone(),
        resource: challenge.resource.clone(),
        expiry: challenge.expiry,
        idempotency_key: idempotency_key.to_string(),
        pubkey_id: signer.key_id().as_str().to_string(),
        op_hash: hash.to_hex(),
        sig: URL_SAFE_NO_PAD.encode(sig.as_bytes()),
    }
}
