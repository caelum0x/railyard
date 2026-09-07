use std::sync::Arc;

use aitp_client::{HoldSettlementAttestation, hold_reference_hash};
use ed25519_dalek::VerifyingKey;
use relay_contract::SettlementEvidence;
use relay_sdk::{EvidenceValidationError, SettlementEvidenceValidator};

use crate::{AitpError, Result};

/// Independently verifies Mint's terminal Ed25519 attestation, then binds its
/// authenticated claims to both Relay's receipt projection and the canonical
/// `relay:{principal}:{request_id}` settlement reference.
///
/// The key id and public key are operator-pinned inputs. They are never learned
/// from the untrusted attestation envelope.
#[derive(Clone)]
pub struct MintAttestationValidator {
    expected_key_id: Arc<str>,
    key: VerifyingKey,
}

impl MintAttestationValidator {
    pub fn from_public_key_bytes(
        expected_key_id: impl Into<Arc<str>>,
        public_key: [u8; 32],
    ) -> Result<Self> {
        let expected_key_id = expected_key_id.into();
        if expected_key_id.trim().is_empty() {
            return Err(AitpError::InvalidAttestation(
                "pinned signer key id is empty".into(),
            ));
        }
        let key = VerifyingKey::from_bytes(&public_key)
            .map_err(|_| AitpError::InvalidAttestation("invalid Ed25519 public key".into()))?;
        Ok(Self {
            expected_key_id,
            key,
        })
    }

    pub fn verify(&self, evidence: &SettlementEvidence) -> Result<()> {
        let value = evidence
            .attestation
            .as_ref()
            .ok_or(AitpError::MissingAttestation)?;
        let attestation: HoldSettlementAttestation = serde_json::from_value(value.clone())
            .map_err(|_| AitpError::InvalidAttestation("invalid envelope shape".into()))?;
        let claims = attestation
            .verify(&self.expected_key_id, &self.key)
            .map_err(|error| AitpError::InvalidAttestation(error.to_string()))?;
        let receipt = &evidence.receipt;
        ensure_equal(claims.sequence == receipt.sequence, "receipt sequence")?;
        ensure_equal(claims.operation == receipt.operation, "receipt operation")?;
        ensure_equal(
            claims.operation_hash == receipt.operation_hash,
            "receipt operation hash",
        )?;
        ensure_equal(
            claims.amount_micros == receipt.amount_micros.get(),
            "receipt amount",
        )?;
        ensure_equal(claims.receipt_hash == receipt.receipt_hash, "receipt hash")?;

        let reference = format!("relay:{}:{}", evidence.principal, evidence.request_id);
        ensure_equal(
            claims.reference_hash == hold_reference_hash(&reference),
            "principal/request reference",
        )
    }
}

impl SettlementEvidenceValidator for MintAttestationValidator {
    fn validate(
        &self,
        evidence: &SettlementEvidence,
    ) -> std::result::Result<(), EvidenceValidationError> {
        self.verify(evidence)
            .map_err(|error| EvidenceValidationError(error.to_string()))
    }
}

fn ensure_equal(matches: bool, field: &'static str) -> Result<()> {
    if matches {
        Ok(())
    } else {
        Err(AitpError::EvidenceMismatch(field))
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine as _;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use ed25519_dalek::{Signer as _, SigningKey};
    use relay_contract::{Microdollars, SettlementReceiptProof};
    use sha2::{Digest, Sha256};
    use uuid::Uuid;

    use super::*;
    use aitp_client::{HOLD_SETTLEMENT_ATTESTATION_FORMAT, HoldSettlementAttestationClaims};

    const KEY_ID: &str = "mint-relay-test-key";

    fn signed_evidence() -> (SettlementEvidence, MintAttestationValidator) {
        let signing = SigningKey::from_bytes(&[9; 32]);
        let request_id = Uuid::parse_str("018f7e23-7b55-7d9a-8000-000000000001").unwrap();
        let principal = "acct_relay_owner".to_string();
        let operation_hash = "11".repeat(32);
        let receipt_hash = "22".repeat(32);
        let claims = HoldSettlementAttestationClaims {
            version: 1,
            asset: "AIT".into(),
            unit: "micro_usd".into(),
            sequence: 52,
            operation: "hold.capture".into(),
            operation_hash: operation_hash.clone(),
            amount_micros: 2_500,
            receipt_hash: receipt_hash.clone(),
            timestamp_ms: 1_700_000_000_456,
            reference_hash: hold_reference_hash(&format!("relay:{principal}:{request_id}")),
            hold_commitment: "33".repeat(32),
            payer_commitment: "44".repeat(32),
            payee_commitment: "55".repeat(32),
            signer_key_id: KEY_ID.into(),
        };
        let payload = serde_json::to_vec(&claims).unwrap();
        let mut hasher = Sha256::new();
        hasher.update(b"mint-hold-settlement-attestation-v1\0");
        hasher.update(&payload);
        let digest: [u8; 32] = hasher.finalize().into();
        let attestation = HoldSettlementAttestation {
            format: HOLD_SETTLEMENT_ATTESTATION_FORMAT.into(),
            payload_base64url: URL_SAFE_NO_PAD.encode(payload),
            digest_sha256: hex::encode(digest),
            signer_key_id: KEY_ID.into(),
            signature_hex: hex::encode(signing.sign(&digest).to_bytes()),
        };
        let evidence = SettlementEvidence {
            request_id,
            principal,
            receipt: SettlementReceiptProof {
                sequence: 52,
                operation: "hold.capture".into(),
                operation_hash,
                amount_micros: Microdollars::new(2_500),
                balance_after_micros: None,
                prev_hash: None,
                receipt_hash,
                signature: "66".repeat(64),
            },
            attestation: Some(serde_json::to_value(attestation).unwrap()),
            verified_at_ms: Some(1_700_000_000_457),
            settled_at_ms: 1_700_000_000_458,
        };
        let validator = MintAttestationValidator::from_public_key_bytes(
            KEY_ID,
            signing.verifying_key().to_bytes(),
        )
        .unwrap();
        (evidence, validator)
    }

    #[test]
    fn verifies_signature_receipt_and_principal_request_binding() {
        let (evidence, validator) = signed_evidence();
        validator.verify(&evidence).unwrap();
    }

    #[test]
    fn rejects_cross_request_replay_and_projection_tampering() {
        let (mut evidence, validator) = signed_evidence();
        evidence.request_id = Uuid::now_v7();
        assert_eq!(
            validator.verify(&evidence),
            Err(AitpError::EvidenceMismatch("principal/request reference"))
        );

        let (mut evidence, validator) = signed_evidence();
        evidence.receipt.amount_micros = Microdollars::new(2_501);
        assert_eq!(
            validator.verify(&evidence),
            Err(AitpError::EvidenceMismatch("receipt amount"))
        );
    }
}
