//! Independent Ed25519 verification of Relay/Mint's signed
//! [`SourceSettlementReceipt`](crate::domain::SourceSettlementReceipt).
//!
//! `MeterEvent::validate` (see [`crate::domain`]) only checks that a receipt's
//! fields are present and correlate with the request's own principal/amount —
//! it is a pure function with no I/O, so it cannot hold key material or make a
//! network call. This module is the layer that actually proves a receipt was
//! issued by Relay/Mint rather than typed in by whoever holds the Ledgerline
//! API key: a receipt is accepted only if `signature` is a genuine Ed25519
//! signature, by one of the keys this deployment trusts, over the raw bytes of
//! the receipt's own `receipt_hash`.
//!
//! This mirrors the pattern already live elsewhere in the monorepo —
//! `context-passport`'s `passport_server::receipts::ReceiptVerifier` and
//! `relay-aitp-wire`'s `HoldSettlementAttestation::verify` both check an
//! Ed25519 signature against a configured or discovered Mint public key before
//! trusting settlement evidence a caller handed them.

use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use serde::Deserialize;

use crate::domain::SourceSettlementReceipt;

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
    /// trusted key, over `receipt.receipt_hash`.
    ///
    /// This does not re-derive `receipt_hash`/`operation_hash` from the
    /// receipt's other fields — `MeterEvent::validate` already checked those
    /// correlate with the request's own `request_id`/`payer_account`/
    /// `operation`/`amount_micros`. This is the layer that checks the receipt
    /// was genuinely issued by Relay/Mint, not simply typed in by whoever
    /// holds the Ledgerline API key.
    pub fn verify(&self, receipt: &SourceSettlementReceipt) -> Result<(), String> {
        let hash: [u8; 32] = hex::decode(receipt.receipt_hash.trim())
            .map_err(|_| "source settlement receipt_hash is not hex".to_string())?
            .try_into()
            .map_err(|_| "source settlement receipt_hash must be a 32-byte hash".to_string())?;
        let sig: [u8; 64] = hex::decode(receipt.signature.trim())
            .map_err(|_| "source settlement signature is not hex".to_string())?
            .try_into()
            .map_err(|_| "source settlement signature must be 64 bytes".to_string())?;
        let signature = Signature::from_bytes(&sig);
        if self
            .keys
            .iter()
            .any(|key| key.verify(&hash, &signature).is_ok())
        {
            Ok(())
        } else {
            Err(
                "source settlement receipt signature does not verify against any trusted \
                 Relay/Mint key"
                    .into(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer as _, SigningKey};

    use super::*;

    fn receipt(receipt_hash: &str, signature: &str) -> SourceSettlementReceipt {
        SourceSettlementReceipt {
            request_id: "req-1".into(),
            payer_account: "acct_customer".into(),
            sequence: 42,
            operation: "hold.capture".into(),
            amount_micros: 1_250_000,
            operation_hash: "op_hash".into(),
            receipt_hash: receipt_hash.into(),
            signature: signature.into(),
        }
    }

    #[test]
    fn accepts_a_genuine_signature_and_rejects_a_forged_one() {
        let signing_key = SigningKey::from_bytes(&[7u8; 32]);
        let verifier = SourceReceiptVerifier::from_hex_keys([hex::encode(
            signing_key.verifying_key().as_bytes(),
        )])
        .expect("valid key");

        let hash = [9u8; 32];
        let genuine_sig = signing_key.sign(&hash);
        let genuine = receipt(&hex::encode(hash), &hex::encode(genuine_sig.to_bytes()));
        assert!(verifier.verify(&genuine).is_ok());

        // Any non-empty string used to satisfy the old shape-only check must
        // now be rejected: it does not verify against the trusted key.
        let fabricated = receipt(&hex::encode(hash), &hex::encode([0u8; 64]));
        assert!(verifier.verify(&fabricated).is_err());

        // A signature produced by a different (untrusted) key must also fail.
        let other_key = SigningKey::from_bytes(&[8u8; 32]);
        let wrong_signer_sig = other_key.sign(&hash);
        let wrong_signer = receipt(
            &hex::encode(hash),
            &hex::encode(wrong_signer_sig.to_bytes()),
        );
        assert!(verifier.verify(&wrong_signer).is_err());
    }

    #[test]
    fn empty_verifier_fails_closed() {
        let verifier = SourceReceiptVerifier::none();
        let receipt = receipt(&hex::encode([1u8; 32]), &hex::encode([2u8; 64]));
        assert!(verifier.verify(&receipt).is_err());
    }

    #[test]
    fn rejects_malformed_hex_fields() {
        let verifier =
            SourceReceiptVerifier::from_hex_keys([hex::encode([3u8; 32])]).expect("valid key");
        assert!(verifier
            .verify(&receipt("not-hex", "also-not-hex"))
            .is_err());
        assert!(verifier
            .verify(&receipt(&hex::encode([1u8; 32]), "ab"))
            .is_err());
    }

    #[test]
    fn from_hex_keys_requires_at_least_one_key() {
        assert!(SourceReceiptVerifier::from_hex_keys(Vec::<String>::new()).is_err());
        assert!(SourceReceiptVerifier::from_hex_keys(["not-hex"]).is_err());
    }
}
