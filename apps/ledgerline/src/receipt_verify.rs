//! Independent Ed25519 verification of Relay/Mint's signed
//! [`SourceSettlementReceipt`](crate::domain::SourceSettlementReceipt).
//!
//! `MeterEvent::validate` (see [`crate::domain`]) only checks that a receipt's
//! fields are present and correlate with the request's own principal/amount —
//! it is a pure function with no I/O, so it cannot hold key material or make a
//! network call. This module is the layer that actually proves a receipt was
//! issued by Relay/Mint rather than typed in by whoever holds the Ledgerline
//! API key.
//!
//! **Residual gap fixed 2026-11-06, before any real producer existed to
//! coordinate around (zero other call sites of `SourceSettlementReceipt`
//! anywhere in this monorepo — confirmed via `grep`):** the original
//! 2026-11-05 fix (`600d177e`) verified `signature` only against the bare
//! `receipt_hash` bytes, with `request_id`/`payer_account`/`operation`/
//! `amount_micros`/`sequence`/`operation_hash` sitting alongside it as
//! separate, unsigned fields. Nothing bound those claim fields to
//! `receipt_hash` itself — `MeterEvent::validate`'s "correlation" check only
//! compares the receipt's fields against the *same request's own* fields
//! (both caller-controlled, so trivially self-consistent), never against an
//! independent ground truth. That meant a caller holding any ONE genuine
//! `(receipt_hash, signature)` pair — e.g. from their own legitimate
//! low-value transaction — could replay that exact pair verbatim while
//! declaring an arbitrary `amount_micros`/`payer_account`/`operation` for an
//! unrelated event: `verify()` would still report a valid signature, because
//! it never checked the signature was over *this* claim. This is the same
//! hash-substitution class the monorepo's own `mint-server::attestation` and
//! `relay-aitp-wire::HoldSettlementAttestation` already defend against by
//! signing a payload that embeds every claim field, with an explicit
//! "callers must additionally compare commitments and amount with their own
//! persisted command" verification contract. Ledgerline's ad hoc
//! `SourceSettlementReceipt` shape never adopted that pattern.
//!
//! Fixed by verifying the signature against [`commitment_digest`] — a SHA-256
//! over ALL of `request_id`/`payer_account`/`sequence`/`operation`/
//! `amount_micros`/`operation_hash`/`receipt_hash`, domain-separated — instead
//! of over `receipt_hash` alone. Changing any one claim field now changes the
//! digest and invalidates the signature, so a genuine signature for one claim
//! can never be replayed to back a different one. This redefines the signing
//! contract for `SourceSettlementReceipt`; safe to do now because no real
//! Relay/Mint producer emits this shape yet (confirmed: zero other call
//! sites), so nothing in production depends on the old, narrower contract.
//!
//! This mirrors the pattern already live elsewhere in the monorepo —
//! `context-passport`'s `passport_server::receipts::ReceiptVerifier` and
//! `relay-aitp-wire`'s `HoldSettlementAttestation::verify` both check an
//! Ed25519 signature against a configured or discovered Mint public key before
//! trusting settlement evidence a caller handed them.

use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::domain::SourceSettlementReceipt;

/// Domain-separation prefix for [`commitment_digest`], so this digest can
/// never collide with a digest computed for an unrelated purpose even if the
/// same bytes happened to be hashed elsewhere.
const COMMITMENT_DOMAIN: &[u8] = b"ledgerline-source-settlement-receipt-commitment-v1\0";

/// The exact bytes a trusted key's signature must cover: every claim field on
/// `receipt`, domain-separated and length-delimited so no concatenation
/// ambiguity exists between adjacent variable-length fields (e.g. a `/`
/// embedded in `request_id` can't be confused with a field boundary).
pub(crate) fn commitment_digest(receipt: &SourceSettlementReceipt) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(COMMITMENT_DOMAIN);
    for field in [
        receipt.request_id.as_bytes(),
        receipt.payer_account.as_bytes(),
        receipt.operation.as_bytes(),
        receipt.operation_hash.as_bytes(),
        receipt.receipt_hash.as_bytes(),
    ] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field);
    }
    hasher.update(receipt.sequence.to_be_bytes());
    hasher.update(receipt.amount_micros.to_be_bytes());
    hasher.finalize().into()
}

/// The set of Relay/Mint receipt-signing public keys this deployment trusts.
///
/// An empty verifier ([`SourceReceiptVerifier::none`]) is intentionally valid
/// to construct: it fails closed, rejecting every receipt, rather than
/// refusing to start or silently accepting unverifiable evidence.
#[derive(Clone, Debug, Default)]
pub struct SourceReceiptVerifier {
    keys: Vec<VerifyingKey>,
}

#[derive(Deserialize)]
struct VerificationKeysDoc {
    keys: Vec<VerificationKeyEntry>,
}

#[derive(Deserialize)]
struct VerificationKeyEntry {
    algorithm: String,
    public_key_hex: String,
    #[serde(default)]
    status: String,
}

impl SourceReceiptVerifier {
    /// Build from hex-encoded 32-byte Ed25519 public keys (e.g. the value of
    /// `LEDGERLINE_SOURCE_RECEIPT_PUBLIC_KEYS`, comma-separated).
    pub fn from_hex_keys<I, S>(keys: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut parsed = Vec::new();
        for raw in keys {
            let raw = raw.as_ref().trim();
            if raw.is_empty() {
                continue;
            }
            let bytes: [u8; 32] = hex::decode(raw)
                .map_err(|error| format!("source receipt public key is not hex: {error}"))?
                .try_into()
                .map_err(|_| "source receipt public key must be 32 bytes".to_string())?;
            let key = VerifyingKey::from_bytes(&bytes).map_err(|error| {
                format!("source receipt public key is not a valid Ed25519 point: {error}")
            })?;
            parsed.push(key);
        }
        if parsed.is_empty() {
            return Err("at least one source receipt public key is required".into());
        }
        Ok(Self { keys: parsed })
    }

    /// Discover the trusted keys from a live Relay/Mint's verification-key
    /// discovery document (`GET {base_url}/.well-known/mint-verification-keys.json`,
    /// the same document `mint-server` already serves). Only
    /// `status == "active"` Ed25519 keys are trusted — a retired key can never
    /// re-validate a new receipt.
    pub async fn fetch(base_url: &str) -> Result<Self, String> {
        let url = format!(
            "{}/.well-known/mint-verification-keys.json",
            base_url.trim_end_matches('/')
        );
        let doc: VerificationKeysDoc = reqwest::get(&url)
            .await
            .map_err(|error| format!("fetching {url}: {error}"))?
            .error_for_status()
            .map_err(|error| format!("fetching {url}: {error}"))?
            .json()
            .await
            .map_err(|error| format!("decoding {url}: {error}"))?;
        Self::from_hex_keys(
            doc.keys
                .into_iter()
                .filter(|key| key.algorithm.eq_ignore_ascii_case("ed25519"))
                .filter(|key| key.status == "active")
                .map(|key| key.public_key_hex),
        )
    }

    /// An always-fail-closed verifier that trusts no key. Every
    /// [`SourceReceiptVerifier::verify`] call on it is rejected. Used when key
    /// configuration/discovery is unavailable, so an unverifiable receipt is
    /// refused rather than silently accepted.
    pub fn none() -> Self {
        Self { keys: Vec::new() }
    }

    /// Verify that `receipt.signature` is a genuine Ed25519 signature, by a
    /// trusted key, over [`commitment_digest(receipt)`](commitment_digest) —
    /// the SHA-256 binding ALL of `receipt`'s claim fields together, not just
    /// its bare `receipt_hash`.
    ///
    /// Binding every field into the signed digest (rather than trusting a
    /// signature over `receipt_hash` alone, with the surrounding fields left
    /// unsigned) is what prevents a genuine signature issued for one
    /// `(request_id, payer_account, operation, amount_micros, ...)` claim
    /// from being replayed to back a *different* claim: changing any one
    /// field changes the digest, which invalidates the signature. This is
    /// the layer that checks the receipt was genuinely issued by Relay/Mint
    /// for *this exact claim*, not simply typed in (or replayed from an
    /// unrelated genuine receipt) by whoever holds the Ledgerline API key.
    pub fn verify(&self, receipt: &SourceSettlementReceipt) -> Result<(), String> {
        if hex::decode(receipt.receipt_hash.trim())
            .ok()
            .map(|bytes| bytes.len())
            != Some(32)
        {
            return Err("source settlement receipt_hash must be a 32-byte hash".into());
        }
        let digest = commitment_digest(receipt);
        let sig: [u8; 64] = hex::decode(receipt.signature.trim())
            .map_err(|_| "source settlement signature is not hex".to_string())?
            .try_into()
            .map_err(|_| "source settlement signature must be 64 bytes".to_string())?;
        let signature = Signature::from_bytes(&sig);
        if self
            .keys
            .iter()
            .any(|key| key.verify(&digest, &signature).is_ok())
        {
            Ok(())
        } else {
            Err(
                "source settlement receipt signature does not verify against any trusted \
                 Relay/Mint key for this exact claim"
                    .into(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer as _, SigningKey};

    use super::*;

    fn receipt(receipt_hash: &str, amount_micros: i64, signature: &str) -> SourceSettlementReceipt {
        SourceSettlementReceipt {
            request_id: "req-1".into(),
            payer_account: "acct_customer".into(),
            sequence: 42,
            operation: "hold.capture".into(),
            amount_micros,
            operation_hash: "op_hash".into(),
            receipt_hash: receipt_hash.into(),
            signature: signature.into(),
        }
    }

    fn sign_for(signing_key: &SigningKey, receipt: &SourceSettlementReceipt) -> String {
        let digest = commitment_digest(receipt);
        hex::encode(signing_key.sign(&digest).to_bytes())
    }

    #[test]
    fn accepts_a_genuine_signature_and_rejects_a_forged_one() {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let verifier = SourceReceiptVerifier::from_hex_keys([hex::encode(
            signing_key.verifying_key().as_bytes(),
        )])
        .expect("valid key");

        let hash = [9u8; 32];
        let genuine = receipt(&hex::encode(hash), 1_250_000, "");
        let genuine_sig = sign_for(&signing_key, &genuine);
        let genuine = receipt(&hex::encode(hash), 1_250_000, &genuine_sig);
        assert!(verifier.verify(&genuine).is_ok());

        // Any non-empty string used to satisfy the old shape-only check must
        // now be rejected: it does not verify against the trusted key.
        let fabricated = receipt(&hex::encode(hash), 1_250_000, &hex::encode([0u8; 64]));
        assert!(verifier.verify(&fabricated).is_err());

        // A signature produced by a different (untrusted) key must also fail.
        let other_key = SigningKey::from_bytes(&[8u8; 32]);
        let wrong_signer_sig = sign_for(&other_key, &genuine);
        let wrong_signer = receipt(&hex::encode(hash), 1_250_000, &wrong_signer_sig);
        assert!(verifier.verify(&wrong_signer).is_err());
    }

    /// The exact residual gap fixed 2026-11-06: the pre-fix code verified
    /// `signature` against the bare `receipt_hash` bytes only, so a genuine
    /// `(receipt_hash, signature)` pair issued for one claim (e.g. a real
    /// $1.25 capture) could be copied verbatim onto a DIFFERENT claim (e.g. a
    /// forged $1,000,000 capture against a different payer) and still "pass"
    /// — the surrounding fields were never part of what the signature
    /// covered. Proves that attack is now closed: the identical genuine
    /// `receipt_hash`+`signature` from a real small transaction, replayed
    /// onto a claim with a different `amount_micros`/`payer_account`, must be
    /// rejected, because changing those fields changes `commitment_digest`
    /// and invalidates the signature.
    #[test]
    fn rejects_a_genuine_receipt_hash_and_signature_replayed_onto_a_different_claim() {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let verifier = SourceReceiptVerifier::from_hex_keys([hex::encode(
            signing_key.verifying_key().as_bytes(),
        )])
        .expect("valid key");

        // A genuine receipt for a real, small, legitimate transaction.
        let hash = [9u8; 32];
        let legitimate = receipt(&hex::encode(hash), 1_250_000, "");
        let legitimate_sig = sign_for(&signing_key, &legitimate);
        let legitimate = receipt(&hex::encode(hash), 1_250_000, &legitimate_sig);
        assert!(
            verifier.verify(&legitimate).is_ok(),
            "the legitimate receipt must itself still verify"
        );

        // The exact same receipt_hash + signature, replayed onto a forged
        // claim for a 1000x larger amount against a different payer. Under
        // the pre-fix (600d177e) code, `verify()` only checked the signature
        // against the bare hash — unchanged here — so this would incorrectly
        // pass. It must now fail.
        let mut forged = legitimate.clone();
        forged.amount_micros = 1_250_000_000;
        forged.payer_account = "acct_victim".into();
        assert!(
            verifier.verify(&forged).is_err(),
            "a genuine receipt_hash+signature must not be replayable onto a different claim"
        );

        // Same attack shape, but only request_id changed (correlates a
        // forged event to someone else's real request).
        let mut forged_request = legitimate.clone();
        forged_request.request_id = "req-victim".into();
        assert!(verifier.verify(&forged_request).is_err());
    }

    #[test]
    fn empty_verifier_fails_closed() {
        let verifier = SourceReceiptVerifier::none();
        let receipt = receipt(&hex::encode([1u8; 32]), 1_250_000, &hex::encode([2u8; 64]));
        assert!(verifier.verify(&receipt).is_err());
    }

    #[test]
    fn rejects_malformed_hex_fields() {
        let verifier =
            SourceReceiptVerifier::from_hex_keys([hex::encode([3u8; 32])]).expect("valid key");
        assert!(verifier
            .verify(&receipt("not-hex", 1_250_000, "also-not-hex"))
            .is_err());
        assert!(verifier
            .verify(&receipt(&hex::encode([1u8; 32]), 1_250_000, "ab"))
            .is_err());
    }

    #[test]
    fn from_hex_keys_requires_at_least_one_key() {
        assert!(SourceReceiptVerifier::from_hex_keys(Vec::<String>::new()).is_err());
        assert!(SourceReceiptVerifier::from_hex_keys(["not-hex"]).is_err());
    }
}
