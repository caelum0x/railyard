//! Cryptographic verification of Mint settlement receipts.
//!
//! Mint signs every receipt with Ed25519 over the 32 raw bytes of the receipt
//! hash (see `mint-receipts`), and publishes its public keys at
//! `GET /.well-known/mint-verification-keys.json`. Identity Lock refuses to
//! record provenance on the strength of settlement evidence it cannot verify:
//! a fabricated `signature: "x"` or a receipt signed by any key other than
//! Mint's is rejected before a GenerationRecord or C2PA manifest exists.
//!
//! **Residual gap fixed 2026-11-07:** [`ReceiptVerifier::verify`] only proves
//! `receipt.signature` is a genuine Mint signature over `receipt.receipt_hash`
//! — it never confirms that hash was actually computed over the
//! `amount_micros`/`operation`/`operation_hash`/`sequence` claims traveling
//! alongside it in the same [`PaymentEvidence`]. Those fields are plain,
//! unsigned JSON on the wire; nothing bound them to `receipt_hash`. Since
//! `check_settlement_evidence` (in `service.rs`) only compares them against
//! each other and against the passport owner — never against independent
//! ground truth — a passport owner attesting their own generation
//! (`POST /v1/passports/:id/attest`) could take ANY genuine
//! `(receipt_hash, signature)` pair from their own real, unrelated,
//! arbitrarily small Mint settlement and replay it verbatim alongside a
//! forged `amount_micros` describing a much larger payment. This is the same
//! vulnerability class fixed in Ledgerline (`531fabbc`,
//! `SourceReceiptVerifier`/`MeterEvent::validate`), which could not simply be
//! redefined the same way here: Ledgerline had zero live producers signing
//! its old, narrower contract, so it was safe to change what bytes the
//! signature covers; Mint's receipt signature here is genuinely live
//! (`receipt.signature` really is `Ed25519(mint_key, receipt_hash)` and
//! nothing else), so redefining it would reject every real Mint receipt.
//!
//! Fixed instead by requiring and verifying [`SettlementAttestationProof`] —
//! Mint's own `TransferAttestation`/`HoldSettlementAttestation`
//! (`mint-server::attestation`), ALREADY built exactly for this purpose and
//! returned in the JSON body of `POST /v1/transfers` and
//! `POST /v1/holds/:id/capture` (`data.attestation`) that any legitimate
//! external caller settling payment directly against Mint already receives.
//! Its signature binds `amount_micros`/`operation`/`operation_hash`/
//! `sequence`/`receipt_hash` together under one SHA-256 digest
//! (`attestation_digest`/`hold_attestation_digest`), so a genuine attestation
//! for one claim can never be replayed to back a different one.
//! [`ReceiptVerifier::verify_bound`] verifies that attestation (reimplementing
//! Mint's exact digest/domain-separation bytes so no `mint-server` dependency
//! is needed, mirroring how `ledgerline::receipt_verify` stays dependency-free
//! too) and cross-checks its claims equal `receipt`'s fields, then `attest`
//! (the externally-reachable surface) calls it instead of the unbound
//! `verify`. `generate`'s evidence still calls plain `verify`: Relay's
//! `/v1/passport/generate` transports only the legacy `AITP-Receipt` header
//! today, not an attestation, and that path stays lower-risk while its
//! evidence originates from the in-process `RelayConditioningSink` seam
//! rather than an external caller.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use sha2::{Digest, Sha256};

use passport_core::{PaymentEvidence, SettlementAttestationProof};

/// Domain-separated SHA-256 prefixes, byte-for-byte identical to
/// `mint-server::attestation::{DIGEST_DOMAIN, HOLD_DIGEST_DOMAIN}`, so a
/// genuine Mint-issued attestation verifies here without any shared
/// dependency.
const TRANSFER_FORMAT: &str = "mint-transfer-attestation-v1";
const TRANSFER_DIGEST_DOMAIN: &[u8] = b"mint-transfer-attestation-v1\0";
const HOLD_FORMAT: &str = "mint-hold-settlement-attestation-v1";
const HOLD_DIGEST_DOMAIN: &[u8] = b"mint-hold-settlement-attestation-v1\0";

/// The claim fields this verifier needs out of an attestation's
/// `payload_base64url`, deliberately a strict subset of Mint's full
/// `TransferAttestationClaims`/`HoldSettlementAttestationClaims` (which also
/// carry `version`/`asset`/`unit`/`timestamp_ms`/commitments this crate has
/// no independent ground truth to check) — `serde` ignores the extra fields.
#[derive(serde::Deserialize)]
struct AttestationClaims {
    sequence: u64,
    operation: String,
    operation_hash: String,
    amount_micros: i64,
    receipt_hash: String,
}

/// The set of Mint receipt-signing public keys this deployment trusts.
#[derive(Clone, Debug)]
pub struct ReceiptVerifier {
    keys: Vec<VerifyingKey>,
}

impl ReceiptVerifier {
    /// Build from hex-encoded 32-byte Ed25519 public keys.
    pub fn from_hex_keys<I, S>(keys: I) -> Result<Self, String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut parsed = Vec::new();
        for key in keys {
            let key = key.as_ref().trim();
            if key.is_empty() {
                continue;
            }
            let bytes: [u8; 32] = hex::decode(key)
                .map_err(|e| format!("Mint public key is not hex: {e}"))?
                .try_into()
                .map_err(|_| "Mint public key must be 32 bytes".to_string())?;
            let vk = VerifyingKey::from_bytes(&bytes)
                .map_err(|e| format!("Mint public key is not a valid Ed25519 point: {e}"))?;
            parsed.push(vk);
        }
        if parsed.is_empty() {
            return Err("at least one Mint receipt public key is required".into());
        }
        Ok(Self { keys: parsed })
    }

    /// Fetch the trusted keys from a live Mint's discovery document.
    pub async fn fetch(mint_base: &str) -> Result<Self, String> {
        #[derive(serde::Deserialize)]
        struct Doc {
            keys: Vec<Key>,
        }
        #[derive(serde::Deserialize)]
        struct Key {
            algorithm: String,
            public_key_hex: String,
            #[serde(default)]
            status: String,
        }
        let url = format!(
            "{}/.well-known/mint-verification-keys.json",
            mint_base.trim_end_matches('/')
        );
        let doc: Doc = reqwest::get(&url)
            .await
            .map_err(|e| format!("fetching {url}: {e}"))?
            .error_for_status()
            .map_err(|e| format!("fetching {url}: {e}"))?
            .json()
            .await
            .map_err(|e| format!("decoding {url}: {e}"))?;
        Self::from_hex_keys(
            doc.keys
                .into_iter()
                .filter(|k| k.algorithm.eq_ignore_ascii_case("ed25519"))
                .filter(|k| k.status.is_empty() || k.status == "active")
                .map(|k| k.public_key_hex),
        )
    }

    /// Verify that `evidence.receipt.signature` is a valid Mint Ed25519
    /// signature over `evidence.receipt.receipt_hash`.
    pub fn verify(&self, evidence: &PaymentEvidence) -> Result<(), String> {
        let hash: [u8; 32] = hex::decode(evidence.receipt.receipt_hash.trim())
            .map_err(|_| "receipt_hash is not hex".to_string())?
            .try_into()
            .map_err(|_| "receipt_hash must be a 32-byte hash".to_string())?;
        let sig: [u8; 64] = hex::decode(evidence.receipt.signature.trim())
            .map_err(|_| "receipt signature is not hex".to_string())?
            .try_into()
            .map_err(|_| "receipt signature must be 64 bytes".to_string())?;
        let signature = Signature::from_bytes(&sig);
        if self
            .keys
            .iter()
            .any(|vk| vk.verify(&hash, &signature).is_ok())
        {
            Ok(())
        } else {
            Err("receipt signature does not verify against any trusted Mint key".into())
        }
    }

    /// Verify `evidence` AND bind it to Mint's signed attestation: proves
    /// `receipt.amount_micros`/`operation`/`operation_hash`/`sequence` are the
    /// exact values Mint signed together with `receipt_hash`, not merely that
    /// `receipt_hash` itself is some genuine (possibly unrelated) receipt.
    /// Required for externally-submitted evidence — see module docs.
    pub fn verify_bound(&self, evidence: &PaymentEvidence) -> Result<(), String> {
        self.verify(evidence)?;
        let attestation = evidence.attestation.as_ref().ok_or_else(|| {
            "payment evidence is missing Mint's signed settlement attestation: \
             receipt_hash alone does not prove amount_micros/operation/sequence \
             were the values Mint actually settled"
                .to_string()
        })?;
        let claims = self.verify_attestation(attestation)?;
        let receipt = &evidence.receipt;
        if claims.receipt_hash != receipt.receipt_hash {
            return Err("attestation receipt_hash does not match the submitted receipt".into());
        }
        if claims.amount_micros != receipt.amount_micros {
            return Err("attestation amount_micros does not match the submitted receipt".into());
        }
        if claims.sequence != receipt.sequence {
            return Err("attestation sequence does not match the submitted receipt".into());
        }
        if claims.operation_hash != receipt.operation_hash {
            return Err("attestation operation_hash does not match the submitted receipt".into());
        }
        if !claims.operation.eq_ignore_ascii_case(&receipt.operation) {
            return Err("attestation operation does not match the submitted receipt".into());
        }
        Ok(())
    }

    /// Verify a [`SettlementAttestationProof`]'s signature over its own
    /// payload bytes (never reserialized — see module docs) and return its
    /// decoded claims. Does not by itself prove anything about a particular
    /// receipt; callers must additionally compare the returned claims against
    /// their own `SettlementReceiptProof` (see [`Self::verify_bound`]).
    fn verify_attestation(
        &self,
        attestation: &SettlementAttestationProof,
    ) -> Result<AttestationClaims, String> {
        let domain = match attestation.format.as_str() {
            TRANSFER_FORMAT => TRANSFER_DIGEST_DOMAIN,
            HOLD_FORMAT => HOLD_DIGEST_DOMAIN,
            other => return Err(format!("unrecognized attestation format: {other}")),
        };
        let payload = URL_SAFE_NO_PAD
            .decode(attestation.payload_base64url.trim())
            .map_err(|_| "attestation payload is not valid base64url".to_string())?;
        let mut hasher = Sha256::new();
        hasher.update(domain);
        hasher.update(&payload);
        let digest: [u8; 32] = hasher.finalize().into();
        if hex::encode(digest) != attestation.digest_sha256 {
            return Err("attestation digest_sha256 does not match its payload".into());
        }
        let sig: [u8; 64] = hex::decode(attestation.signature_hex.trim())
            .map_err(|_| "attestation signature is not hex".to_string())?
            .try_into()
            .map_err(|_| "attestation signature must be 64 bytes".to_string())?;
        let signature = Signature::from_bytes(&sig);
        if !self
            .keys
            .iter()
            .any(|vk| vk.verify(&digest, &signature).is_ok())
        {
            return Err(
                "attestation signature does not verify against any trusted Mint key".into(),
            );
        }
        serde_json::from_slice(&payload)
            .map_err(|_| "attestation payload is not valid claims JSON".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use passport_core::SettlementReceiptProof;

    fn evidence(hash: &str, sig: &str) -> PaymentEvidence {
        PaymentEvidence {
            request_id: "req_1".into(),
            payer_account: "acct".into(),
            receipt: SettlementReceiptProof {
                sequence: 1,
                operation: "hold.capture".into(),
                operation_hash: "op".into(),
                amount_micros: 10,
                receipt_hash: hash.into(),
                signature: sig.into(),
            },
            attestation: None,
        }
    }

    /// Build a genuine `SettlementAttestationProof` the way Mint would,
    /// signed by `sk` over the domain-separated digest of these exact claims.
    #[allow(clippy::too_many_arguments)]
    fn signed_attestation(
        sk: &SigningKey,
        format: &str,
        domain: &[u8],
        sequence: u64,
        operation: &str,
        operation_hash: &str,
        amount_micros: i64,
        receipt_hash: &str,
    ) -> SettlementAttestationProof {
        let payload = serde_json::to_vec(&serde_json::json!({
            "sequence": sequence,
            "operation": operation,
            "operation_hash": operation_hash,
            "amount_micros": amount_micros,
            "receipt_hash": receipt_hash,
        }))
        .unwrap();
        let mut hasher = Sha256::new();
        hasher.update(domain);
        hasher.update(&payload);
        let digest: [u8; 32] = hasher.finalize().into();
        let signature = sk.sign(&digest);
        SettlementAttestationProof {
            format: format.into(),
            payload_base64url: URL_SAFE_NO_PAD.encode(&payload),
            digest_sha256: hex::encode(digest),
            signer_key_id: "mint-verifier-key-1".into(),
            signature_hex: hex::encode(signature.to_bytes()),
        }
    }

    #[test]
    fn accepts_mint_signed_and_rejects_forged() {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let verifier =
            ReceiptVerifier::from_hex_keys([hex::encode(sk.verifying_key().as_bytes())]).unwrap();
        let hash = [9u8; 32];
        let sig = sk.sign(&hash);
        let good = evidence(&hex::encode(hash), &hex::encode(sig.to_bytes()));
        assert!(verifier.verify(&good).is_ok());
        assert!(verifier.verify(&evidence(&hex::encode(hash), "x")).is_err());
        let other = SigningKey::from_bytes(&[8u8; 32]).sign(&hash);
        assert!(verifier
            .verify(&evidence(
                &hex::encode(hash),
                &hex::encode(other.to_bytes())
            ))
            .is_err());
    }

    #[test]
    fn verify_bound_accepts_a_genuine_attestation_bound_to_the_receipt() {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let verifier =
            ReceiptVerifier::from_hex_keys([hex::encode(sk.verifying_key().as_bytes())]).unwrap();
        let hash = [9u8; 32];
        let sig = sk.sign(&hash);
        let mut ev = evidence(&hex::encode(hash), &hex::encode(sig.to_bytes()));
        ev.attestation = Some(signed_attestation(
            &sk,
            HOLD_FORMAT,
            HOLD_DIGEST_DOMAIN,
            ev.receipt.sequence,
            &ev.receipt.operation,
            &ev.receipt.operation_hash,
            ev.receipt.amount_micros,
            &ev.receipt.receipt_hash,
        ));
        assert!(verifier.verify_bound(&ev).is_ok());
    }

    #[test]
    fn verify_bound_rejects_evidence_missing_an_attestation() {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let verifier =
            ReceiptVerifier::from_hex_keys([hex::encode(sk.verifying_key().as_bytes())]).unwrap();
        let hash = [9u8; 32];
        let sig = sk.sign(&hash);
        let ev = evidence(&hex::encode(hash), &hex::encode(sig.to_bytes()));
        assert!(ev.attestation.is_none());
        assert!(verifier.verify_bound(&ev).is_err());
    }

    /// The exact attack the 2026-11-07 fix closes: a genuine
    /// `(receipt_hash, signature)` pair AND its genuine, correctly-signed
    /// attestation, both from a real small settlement, replayed onto evidence
    /// claiming a much larger `amount_micros`. The receipt-level signature
    /// alone (bare `verify`) cannot catch this — it must now be rejected by
    /// `verify_bound`'s cross-check against the attested claims.
    #[test]
    fn verify_bound_rejects_a_genuine_attestation_replayed_onto_a_forged_amount() {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let verifier =
            ReceiptVerifier::from_hex_keys([hex::encode(sk.verifying_key().as_bytes())]).unwrap();
        let hash = [9u8; 32];
        let sig = sk.sign(&hash);
        let legitimate = evidence(&hex::encode(hash), &hex::encode(sig.to_bytes()));
        let attestation = signed_attestation(
            &sk,
            HOLD_FORMAT,
            HOLD_DIGEST_DOMAIN,
            legitimate.receipt.sequence,
            &legitimate.receipt.operation,
            &legitimate.receipt.operation_hash,
            legitimate.receipt.amount_micros,
            &legitimate.receipt.receipt_hash,
        );
        let mut legitimate = legitimate;
        legitimate.attestation = Some(attestation.clone());
        assert!(
            verifier.verify_bound(&legitimate).is_ok(),
            "the legitimate evidence must itself still verify"
        );

        let mut forged = legitimate;
        forged.receipt.amount_micros = 1_000_000_000;
        forged.attestation = Some(attestation);
        assert!(
            verifier.verify_bound(&forged).is_err(),
            "a genuine receipt_hash+signature+attestation must not be replayable onto a forged amount"
        );
    }

    #[test]
    fn verify_bound_rejects_an_attestation_signed_by_an_untrusted_key() {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let other = SigningKey::from_bytes(&[8u8; 32]);
        let verifier =
            ReceiptVerifier::from_hex_keys([hex::encode(sk.verifying_key().as_bytes())]).unwrap();
        let hash = [9u8; 32];
        let sig = sk.sign(&hash);
        let mut ev = evidence(&hex::encode(hash), &hex::encode(sig.to_bytes()));
        ev.attestation = Some(signed_attestation(
            &other,
            HOLD_FORMAT,
            HOLD_DIGEST_DOMAIN,
            ev.receipt.sequence,
            &ev.receipt.operation,
            &ev.receipt.operation_hash,
            ev.receipt.amount_micros,
            &ev.receipt.receipt_hash,
        ));
        assert!(verifier.verify_bound(&ev).is_err());
    }

    #[test]
    fn verify_bound_rejects_an_unrecognized_attestation_format() {
        let sk = SigningKey::from_bytes(&[7u8; 32]);
        let verifier =
            ReceiptVerifier::from_hex_keys([hex::encode(sk.verifying_key().as_bytes())]).unwrap();
        let hash = [9u8; 32];
        let sig = sk.sign(&hash);
        let mut ev = evidence(&hex::encode(hash), &hex::encode(sig.to_bytes()));
        ev.attestation = Some(signed_attestation(
            &sk,
            "not-a-real-format",
            HOLD_DIGEST_DOMAIN,
            ev.receipt.sequence,
            &ev.receipt.operation,
            &ev.receipt.operation_hash,
            ev.receipt.amount_micros,
            &ev.receipt.receipt_hash,
        ));
        assert!(verifier.verify_bound(&ev).is_err());
    }
}
