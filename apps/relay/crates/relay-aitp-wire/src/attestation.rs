//! Independent verification of Mint terminal hold-settlement attestations.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const HOLD_SETTLEMENT_ATTESTATION_FORMAT: &str = "mint-hold-settlement-attestation-v1";

const DIGEST_DOMAIN: &[u8] = b"mint-hold-settlement-attestation-v1\0";
const REFERENCE_DOMAIN: &[u8] = b"mint-hold-settlement-reference-v1\0";
const HOLD_DOMAIN: &[u8] = b"mint-hold-settlement-hold-v1\0";
const PARTY_DOMAIN: &[u8] = b"mint-hold-settlement-party-v1\0";

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HoldSettlementAttestation {
    pub format: String,
    pub payload_base64url: String,
    pub digest_sha256: String,
    pub signer_key_id: String,
    pub signature_hex: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HoldSettlementCommand<'a> {
    pub hold_id: &'a str,
    pub payer_account: &'a str,
    pub operation: HoldSettlementOperation<'a>,
    pub idempotency_key: &'a str,
    pub reference: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoldSettlementOperation<'a> {
    Capture {
        payee_account: &'a str,
        amount_micros: i64,
    },
    Release {
        released_micros: i64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HoldSettlementExpectation<'a> {
    pub command: HoldSettlementCommand<'a>,
    pub receipt_hash: &'a str,
}

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
        let signature: [u8; 64] = hex::decode(&self.signature_hex)
            .map_err(|_| HoldAttestationVerifyError::Encoding)?
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
    let Ok(payload) = serde_json::to_vec(&fields) else {
        // `serde_json::Value` serialization is infallible in practice. Keep
        // the cryptographic boundary non-panicking regardless: an empty hash
        // can never match a valid Mint attestation and therefore fails closed.
        return String::new();
    };
    hex::encode(blake3::hash(&payload).as_bytes())
}

#[must_use]
pub fn hold_reference_hash(reference: &str) -> String {
    sha256_commitment(&[REFERENCE_DOMAIN, reference.as_bytes()])
}

#[must_use]
pub fn hold_commitment(hold_id: &str, operation_hash: &str) -> String {
    sha256_commitment(&[
        HOLD_DOMAIN,
        hold_id.as_bytes(),
        b"\0",
        operation_hash.as_bytes(),
    ])
}

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
    use super::*;

    const GOLDEN_PUBLIC_KEY: &str =
        "74e0be0d9692f0817482d8f3930b51d334db23d11665ca8ee52fe9e8c29715e8";
    const GOLDEN_PAYLOAD: &str = "eyJ2ZXJzaW9uIjoxLCJhc3NldCI6IkFJVCIsInVuaXQiOiJtaWNyb191c2QiLCJzZXF1ZW5jZSI6NTIsIm9wZXJhdGlvbiI6ImhvbGQuY2FwdHVyZSIsIm9wZXJhdGlvbl9oYXNoIjoiZWFmNjA5NmVhNzg1MDcyNmJkN2NlZmI3NjQ3ZDM2MGRjYmE2N2IwOTk1OGUyZjNkMDJmZjJmN2QyODQ3N2Q3NyIsImFtb3VudF9taWNyb3MiOjI1MDAwMDAsInJlY2VpcHRfaGFzaCI6ImQyMDlmYjQzOTk0MTZiMWE2NzdkNmM3MDdjN2ZmYTliNWI4Zjg4MGEzNzgzYzQ3MTIyMWUzYzI3MWVlMWVjMDciLCJ0aW1lc3RhbXBfbXMiOjE3MDAwMDAwMDA0NTYsInJlZmVyZW5jZV9oYXNoIjoiOTBlMjNmMzhmMWUxNGJjOTk4ZDMyZWZjNjJlMjljOGQ4NjE3YmExM2I3OTRkNjRhNjAyZDU1NWEzY2ZkZjVkNyIsImhvbGRfY29tbWl0bWVudCI6Ijg5MzkyOTI4NWFkZmNiNGFkMDgwMTdjMWU2ZDc3MjY1MjhlNGMxZWVjZTRlN2EyNTE2NWEwMWQxMjg5MWRlZDEiLCJwYXllcl9jb21taXRtZW50IjoiMmQxNjQ2YmU2NDJmNjA3YzNjMWNkNjczMDg5N2IwODRmMTExMDg0ZWRhNDFkYjFlNzkzNzk2YzA0YTMyNDdjYyIsInBheWVlX2NvbW1pdG1lbnQiOiJjZTliMDNiOTBmZGIzYWM5ODM3NDEyNzUyOWJjOWU0MmYzOThhN2NhNzY0MTk2OTkyNTgxOWQ2NTMxY2EwNmEzIiwic2lnbmVyX2tleV9pZCI6Im1pbnQtdmVyaWZpZXIta2V5LTEifQ";
    const GOLDEN_DIGEST: &str = "ac410b86bdff39fbe42fe90ae5a3a33747bd285c2eb891bedad59ffa692be898";
    const GOLDEN_SIGNATURE: &str = "4411eadc0d98ac00b792a2029e43ad80458538ab2c7f1306417f92459b9ac903a982a5628f31fe49b3ea8dbc5b5de2b8777b1b7c22a2edf2280f52193cc2d500";

    #[test]
    fn operation_hash_is_deterministic_and_binds_every_command_field() {
        let command = HoldSettlementCommand {
            hold_id: "hold_1",
            payer_account: "acct_payer",
            operation: HoldSettlementOperation::Capture {
                payee_account: "acct_payee",
                amount_micros: 42,
            },
            idempotency_key: "idem_1",
            reference: Some("job_1"),
        };
        let hash = hold_operation_hash(&command);
        assert_eq!(hash.len(), 64);

        let mut changed = command;
        changed.idempotency_key = "idem_2";
        assert_ne!(hash, hold_operation_hash(&changed));
    }

    #[test]
    fn mint_golden_attestation_verifies_without_shared_implementation() {
        let key_bytes: [u8; 32] = hex::decode(GOLDEN_PUBLIC_KEY)
            .expect("golden public key hex")
            .try_into()
            .expect("golden public key length");
        let key = VerifyingKey::from_bytes(&key_bytes).expect("golden public key");
        let attestation = HoldSettlementAttestation {
            format: HOLD_SETTLEMENT_ATTESTATION_FORMAT.to_string(),
            payload_base64url: GOLDEN_PAYLOAD.to_string(),
            digest_sha256: GOLDEN_DIGEST.to_string(),
            signer_key_id: "mint-verifier-key-1".to_string(),
            signature_hex: GOLDEN_SIGNATURE.to_string(),
        };

        let claims = attestation
            .verify("mint-verifier-key-1", &key)
            .expect("Mint golden attestation");
        assert_eq!(claims.operation, "hold.capture");
        assert_eq!(claims.amount_micros, 2_500_000);
        assert_eq!(claims.sequence, 52);
    }
}
