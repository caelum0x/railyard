//! Independent verification for Mint terminal hold-settlement attestations.
//!
//! Mint signs the exact transmitted JSON payload bytes, not a verifier's
//! reserialization. This module therefore decodes `payload_base64url`, hashes
//! those bytes with Mint's domain separator, verifies the Ed25519 signature,
//! and only then parses and validates the claims. It intentionally has no
//! dependency on `mint-server`.

use std::collections::BTreeMap;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Wire-format name emitted by Mint for capture/release attestations.
pub const HOLD_SETTLEMENT_ATTESTATION_FORMAT: &str = "mint-hold-settlement-attestation-v1";

const DIGEST_DOMAIN: &[u8] = b"mint-hold-settlement-attestation-v1\0";
const REFERENCE_DOMAIN: &[u8] = b"mint-hold-settlement-reference-v1\0";
const HOLD_DOMAIN: &[u8] = b"mint-hold-settlement-hold-v1\0";
const PARTY_DOMAIN: &[u8] = b"mint-hold-settlement-party-v1\0";

/// Exact ordered claims carried in a Mint hold-settlement attestation.
///
/// Declaration order mirrors Mint's encoder. Verification never relies on
/// reserializing this value: the signature covers the decoded payload bytes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoldSettlementAttestationClaims {
    pub version: u8,
    pub asset: String,
    pub unit: String,
    pub sequence: u64,
    pub operation: String,
    pub operation_hash: String,
    pub amount_micros: i64,
    pub receipt_hash: String,
    pub timestamp_ms: i64,
    pub reference_hash: String,
    pub hold_commitment: String,
    pub payer_commitment: String,
    pub payee_commitment: String,
    pub signer_key_id: String,
}

/// Privacy-safe Ed25519 envelope returned by Mint on hold capture/release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoldSettlementAttestation {
    pub format: String,
    pub payload_base64url: String,
    pub digest_sha256: String,
    pub signer_key_id: String,
    pub signature_hex: String,
}

/// The terminal command a caller persisted before asking Mint to settle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HoldSettlementCommand<'a> {
    pub hold_id: &'a str,
    pub payer_account: &'a str,
    pub operation: HoldSettlementOperation<'a>,
    pub idempotency_key: &'a str,
    pub reference: Option<&'a str>,
}

/// Capture or release details needed to bind an attestation to local intent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldSettlementOperation<'a> {
    Capture {
        payee_account: &'a str,
        amount_micros: i64,
    },
    /// Mint's release receipt reports the amount returned to availability.
    Release { released_micros: i64 },
}

/// A complete local expectation, including the full receipt hash the compact
/// attestation must authenticate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HoldSettlementExpectation<'a> {
    pub command: HoldSettlementCommand<'a>,
    pub receipt_hash: &'a str,
}

/// Fail-closed verification failures. The variants avoid carrying private
/// account or hold identifiers into logs.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
pub enum HoldAttestationVerifyError {
    #[error("unsupported hold-settlement attestation format")]
    Format,
    #[error("invalid hold-settlement attestation encoding")]
    Encoding,
    #[error("hold-settlement attestation digest mismatch")]
    Digest,
    #[error("hold-settlement attestation signer key mismatch")]
    KeyId,
    #[error("hold-settlement attestation signature invalid")]
    Signature,
    #[error("hold-settlement attestation claims invalid")]
    Claims,
    #[error("hold-settlement attestation does not bind the expected command")]
    Command,
}

impl HoldSettlementAttestation {
    /// Verify the exact payload digest/signature and strict claims shape.
    ///
    /// `expected_key_id` and `key` must come from a caller-pinned trust source;
    /// the envelope's own key id is never sufficient trust discovery.
    pub fn verify(
        &self,
        expected_key_id: &str,
        key: &VerifyingKey,
    ) -> Result<HoldSettlementAttestationClaims, HoldAttestationVerifyError> {
        if self.format != HOLD_SETTLEMENT_ATTESTATION_FORMAT {
            return Err(HoldAttestationVerifyError::Format);
        }
        if self.signer_key_id != expected_key_id {
            return Err(HoldAttestationVerifyError::KeyId);
        }

        let payload = URL_SAFE_NO_PAD
            .decode(&self.payload_base64url)
            .map_err(|_| HoldAttestationVerifyError::Encoding)?;
        let digest = hold_attestation_digest(&payload);
        if hex::encode(digest) != self.digest_sha256 {
            return Err(HoldAttestationVerifyError::Digest);
        }

        let signature =
            hex::decode(&self.signature_hex).map_err(|_| HoldAttestationVerifyError::Encoding)?;
        let signature: [u8; 64] = signature
            .try_into()
            .map_err(|_| HoldAttestationVerifyError::Encoding)?;
        key.verify(&digest, &Signature::from_bytes(&signature))
            .map_err(|_| HoldAttestationVerifyError::Signature)?;

        let claims: HoldSettlementAttestationClaims =
            serde_json::from_slice(&payload).map_err(|_| HoldAttestationVerifyError::Claims)?;
        if claims.version != 1
            || claims.asset != "AIT"
            || claims.unit != "micro_usd"
            || !matches!(claims.operation.as_str(), "hold.capture" | "hold.release")
            || claims.amount_micros <= 0
            || !is_lower_hex_32(&claims.operation_hash)
            || !is_lower_hex_32(&claims.receipt_hash)
            || !is_lower_hex_32(&claims.reference_hash)
            || !is_lower_hex_32(&claims.hold_commitment)
            || !is_lower_hex_32(&claims.payer_commitment)
            || !is_lower_hex_32(&claims.payee_commitment)
            || claims.signer_key_id != self.signer_key_id
        {
            return Err(HoldAttestationVerifyError::Claims);
        }
        Ok(claims)
    }

    /// Verify the envelope, then bind its commitments to one persisted command.
    pub fn verify_expected(
        &self,
        expected_key_id: &str,
        key: &VerifyingKey,
        expected: &HoldSettlementExpectation<'_>,
    ) -> Result<HoldSettlementAttestationClaims, HoldAttestationVerifyError> {
        let claims = self.verify(expected_key_id, key)?;
        claims.verify_expected(expected)?;
        Ok(claims)
    }
}

impl HoldSettlementAttestationClaims {
    /// Compare authenticated claims with locally persisted command inputs.
    pub fn verify_expected(
        &self,
        expected: &HoldSettlementExpectation<'_>,
    ) -> Result<(), HoldAttestationVerifyError> {
        let command = &expected.command;
        let operation_hash = hold_operation_hash(command);
        let (operation, amount_micros, payee_commitment) = match command.operation {
            HoldSettlementOperation::Capture {
                payee_account,
                amount_micros,
            } => (
                "hold.capture",
                amount_micros,
                hold_party_commitment("payee", payee_account, &operation_hash),
            ),
            HoldSettlementOperation::Release { released_micros } => (
                "hold.release",
                released_micros,
                no_payee_commitment(&operation_hash),
            ),
        };

        if amount_micros <= 0
            || self.operation != operation
            || self.operation_hash != operation_hash
            || self.amount_micros != amount_micros
            || self.receipt_hash != expected.receipt_hash
            || self.reference_hash != hold_reference_hash(command.reference.unwrap_or_default())
            || self.hold_commitment != hold_commitment(command.hold_id, &operation_hash)
            || self.payer_commitment
                != hold_party_commitment("payer", command.payer_account, &operation_hash)
            || self.payee_commitment != payee_commitment
        {
            return Err(HoldAttestationVerifyError::Command);
        }
        Ok(())
    }
}

/// Mint-compatible canonical operation hash for capture/release intent.
#[must_use]
pub fn hold_operation_hash(command: &HoldSettlementCommand<'_>) -> String {
    let mut fields: BTreeMap<&str, Value> = BTreeMap::new();
    match command.operation {
        HoldSettlementOperation::Capture {
            payee_account,
            amount_micros,
        } => {
            fields.insert("op", Value::from("hold.capture"));
            fields.insert("hold_id", Value::from(command.hold_id));
            fields.insert("amount_micros", Value::from(amount_micros));
            fields.insert("to", Value::from(payee_account));
        }
        HoldSettlementOperation::Release { .. } => {
            fields.insert("op", Value::from("hold.release"));
            fields.insert("hold_id", Value::from(command.hold_id));
        }
    }
    fields.insert("idem_key", Value::from(command.idempotency_key));
    fields.insert(
        "ref",
        command.reference.map(Value::from).unwrap_or(Value::Null),
    );
    let payload = serde_json::to_vec(&fields).expect("flat hold command always serializes");
    hex::encode(blake3::hash(&payload).as_bytes())
}

/// Domain-separated commitment to the caller's opaque settlement reference.
#[must_use]
pub fn hold_reference_hash(reference: &str) -> String {
    sha256_commitment(&[REFERENCE_DOMAIN, reference.as_bytes()])
}

/// Domain-separated commitment to a Mint hold id and exact operation hash.
#[must_use]
pub fn hold_commitment(hold_id: &str, operation_hash: &str) -> String {
    sha256_commitment(&[
        HOLD_DOMAIN,
        hold_id.as_bytes(),
        b"\0",
        operation_hash.as_bytes(),
    ])
}

/// Domain-separated, role-bound account commitment for one operation.
#[must_use]
pub fn hold_party_commitment(role: &str, account: &str, operation_hash: &str) -> String {
    sha256_commitment(&[
        PARTY_DOMAIN,
        role.as_bytes(),
        b"\0",
        account.as_bytes(),
        b"\0",
        operation_hash.as_bytes(),
    ])
}

/// Explicit no-payee commitment used by release attestations.
#[must_use]
pub fn no_payee_commitment(operation_hash: &str) -> String {
    sha256_commitment(&[PARTY_DOMAIN, b"no-payee\0", operation_hash.as_bytes()])
}

fn hold_attestation_digest(payload: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(DIGEST_DOMAIN);
    hash.update(payload);
    hash.finalize().into()
}

fn sha256_commitment(parts: &[&[u8]]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update(part);
    }
    hex::encode(hash.finalize())
}

fn is_lower_hex_32(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer as _, SigningKey};
    use serde_json::json;

    use super::*;

    const GOLDEN_PUBLIC_KEY: &str =
        "74e0be0d9692f0817482d8f3930b51d334db23d11665ca8ee52fe9e8c29715e8";
    const GOLDEN_PAYLOAD: &str = "eyJ2ZXJzaW9uIjoxLCJhc3NldCI6IkFJVCIsInVuaXQiOiJtaWNyb191c2QiLCJzZXF1ZW5jZSI6NTIsIm9wZXJhdGlvbiI6ImhvbGQuY2FwdHVyZSIsIm9wZXJhdGlvbl9oYXNoIjoiZWFmNjA5NmVhNzg1MDcyNmJkN2NlZmI3NjQ3ZDM2MGRjYmE2N2IwOTk1OGUyZjNkMDJmZjJmN2QyODQ3N2Q3NyIsImFtb3VudF9taWNyb3MiOjI1MDAwMDAsInJlY2VpcHRfaGFzaCI6ImQyMDlmYjQzOTk0MTZiMWE2NzdkNmM3MDdjN2ZmYTliNWI4Zjg4MGEzNzgzYzQ3MTIyMWUzYzI3MWVlMWVjMDciLCJ0aW1lc3RhbXBfbXMiOjE3MDAwMDAwMDA0NTYsInJlZmVyZW5jZV9oYXNoIjoiOTBlMjNmMzhmMWUxNGJjOTk4ZDMyZWZjNjJlMjljOGQ4NjE3YmExM2I3OTRkNjRhNjAyZDU1NWEzY2ZkZjVkNyIsImhvbGRfY29tbWl0bWVudCI6Ijg5MzkyOTI4NWFkZmNiNGFkMDgwMTdjMWU2ZDc3MjY1MjhlNGMxZWVjZTRlN2EyNTE2NWEwMWQxMjg5MWRlZDEiLCJwYXllcl9jb21taXRtZW50IjoiMmQxNjQ2YmU2NDJmNjA3YzNjMWNkNjczMDg5N2IwODRmMTExMDg0ZWRhNDFkYjFlNzkzNzk2YzA0YTMyNDdjYyIsInBheWVlX2NvbW1pdG1lbnQiOiJjZTliMDNiOTBmZGIzYWM5ODM3NDEyNzUyOWJjOWU0MmYzOThhN2NhNzY0MTk2OTkyNTgxOWQ2NTMxY2EwNmEzIiwic2lnbmVyX2tleV9pZCI6Im1pbnQtdmVyaWZpZXIta2V5LTEifQ";
    const GOLDEN_DIGEST: &str = "ac410b86bdff39fbe42fe90ae5a3a33747bd285c2eb891bedad59ffa692be898";
    const GOLDEN_SIGNATURE: &str = "4411eadc0d98ac00b792a2029e43ad80458538ab2c7f1306417f92459b9ac903a982a5628f31fe49b3ea8dbc5b5de2b8777b1b7c22a2edf2280f52193cc2d500";

    fn golden_attestation() -> HoldSettlementAttestation {
        HoldSettlementAttestation {
            format: HOLD_SETTLEMENT_ATTESTATION_FORMAT.to_string(),
            payload_base64url: GOLDEN_PAYLOAD.to_string(),
            digest_sha256: GOLDEN_DIGEST.to_string(),
            signer_key_id: "mint-verifier-key-1".to_string(),
            signature_hex: GOLDEN_SIGNATURE.to_string(),
        }
    }

    fn golden_key() -> VerifyingKey {
        let bytes: [u8; 32] = hex::decode(GOLDEN_PUBLIC_KEY).unwrap().try_into().unwrap();
        VerifyingKey::from_bytes(&bytes).unwrap()
    }

    fn golden_expectation<'a>() -> HoldSettlementExpectation<'a> {
        HoldSettlementExpectation {
            command: HoldSettlementCommand {
                hold_id: "hold_00000000000000000000000003",
                payer_account: "acct_000000000000000000000000004",
                operation: HoldSettlementOperation::Capture {
                    payee_account: "acct_400000000000000000000000005",
                    amount_micros: 2_500_000,
                },
                idempotency_key: "job_01:capture",
                reference: Some("menivor:generation:job_01"),
            },
            receipt_hash: "d209fb4399416b1a677d6c707c7ffa9b5b8f880a3783c471221e3c271ee1ec07",
        }
    }

    #[test]
    fn mint_golden_capture_verifies_and_binds_exact_command() {
        let claims = golden_attestation()
            .verify_expected("mint-verifier-key-1", &golden_key(), &golden_expectation())
            .unwrap();
        assert_eq!(claims.operation, "hold.capture");
        assert_eq!(claims.amount_micros, 2_500_000);
        assert_eq!(claims.sequence, 52);
    }

    #[test]
    fn payload_digest_signature_and_command_tampering_fail_closed() {
        let key = golden_key();

        let mut payload = golden_attestation();
        payload.payload_base64url.push('A');
        assert!(matches!(
            payload.verify("mint-verifier-key-1", &key),
            Err(HoldAttestationVerifyError::Digest | HoldAttestationVerifyError::Encoding)
        ));

        let mut signature = golden_attestation();
        signature.signature_hex.replace_range(..2, "00");
        assert_eq!(
            signature.verify("mint-verifier-key-1", &key),
            Err(HoldAttestationVerifyError::Signature)
        );

        let mut wrong = golden_expectation();
        wrong.command.hold_id = "hold_another";
        assert_eq!(
            golden_attestation().verify_expected("mint-verifier-key-1", &key, &wrong),
            Err(HoldAttestationVerifyError::Command)
        );
    }

    #[test]
    fn valid_signature_with_invalid_claim_shape_is_rejected() {
        let signing = SigningKey::from_bytes(&[9; 32]);
        let payload = serde_json::to_vec(&json!({
            "version": 1,
            "asset": "AIT",
            "unit": "micro_usd",
            "sequence": 1,
            "operation": "hold.capture",
            "operation_hash": "00".repeat(32),
            "amount_micros": 1,
            "receipt_hash": "11".repeat(32),
            "timestamp_ms": 1,
            "reference_hash": "22".repeat(32),
            "hold_commitment": "33".repeat(32),
            "payer_commitment": "44".repeat(32),
            "payee_commitment": "55".repeat(32),
            "signer_key_id": "test-key",
            "unexpected": true
        }))
        .unwrap();
        let digest = hold_attestation_digest(&payload);
        let evidence = HoldSettlementAttestation {
            format: HOLD_SETTLEMENT_ATTESTATION_FORMAT.to_string(),
            payload_base64url: URL_SAFE_NO_PAD.encode(payload),
            digest_sha256: hex::encode(digest),
            signer_key_id: "test-key".to_string(),
            signature_hex: hex::encode(signing.sign(&digest).to_bytes()),
        };
        assert_eq!(
            evidence.verify("test-key", &signing.verifying_key()),
            Err(HoldAttestationVerifyError::Claims)
        );
    }

    #[test]
    fn valid_release_verifies_and_binds_explicit_no_payee() {
        let command = HoldSettlementCommand {
            hold_id: "hold_1",
            payer_account: "acct_payer",
            operation: HoldSettlementOperation::Release {
                released_micros: 42,
            },
            idempotency_key: "job_1:release",
            reference: Some("job_1"),
        };
        let operation_hash = hold_operation_hash(&command);
        let receipt_hash = "11".repeat(32);
        let claims = HoldSettlementAttestationClaims {
            version: 1,
            asset: "AIT".to_string(),
            unit: "micro_usd".to_string(),
            sequence: 7,
            operation: "hold.release".to_string(),
            operation_hash: operation_hash.clone(),
            amount_micros: 42,
            receipt_hash: receipt_hash.clone(),
            timestamp_ms: 1_700_000_000_000,
            reference_hash: hold_reference_hash("job_1"),
            hold_commitment: hold_commitment("hold_1", &operation_hash),
            payer_commitment: hold_party_commitment("payer", "acct_payer", &operation_hash),
            payee_commitment: no_payee_commitment(&operation_hash),
            signer_key_id: "release-key".to_string(),
        };
        let payload = serde_json::to_vec(&claims).unwrap();
        let digest = hold_attestation_digest(&payload);
        let signing = SigningKey::from_bytes(&[13; 32]);
        let evidence = HoldSettlementAttestation {
            format: HOLD_SETTLEMENT_ATTESTATION_FORMAT.to_string(),
            payload_base64url: URL_SAFE_NO_PAD.encode(payload),
            digest_sha256: hex::encode(digest),
            signer_key_id: "release-key".to_string(),
            signature_hex: hex::encode(signing.sign(&digest).to_bytes()),
        };
        let expected = HoldSettlementExpectation {
            command,
            receipt_hash: &receipt_hash,
        };
        assert_eq!(
            evidence
                .verify_expected("release-key", &signing.verifying_key(), &expected)
                .unwrap(),
            claims
        );
        assert_ne!(
            no_payee_commitment(&operation_hash),
            hold_party_commitment("payee", "acct_payee", &operation_hash)
        );

        let mut wrong_amount = expected;
        wrong_amount.command.operation = HoldSettlementOperation::Release {
            released_micros: 41,
        };
        assert_eq!(
            evidence.verify_expected("release-key", &signing.verifying_key(), &wrong_amount),
            Err(HoldAttestationVerifyError::Command)
        );
    }
}
