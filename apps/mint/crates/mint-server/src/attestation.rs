//! Privacy-safe, independently verifiable transfer and hold-settlement evidence.
//!
//! The transmitted payload is the exact compact JSON encoding of
//! the corresponding ordered claims struct. It is carried as unpadded URL-safe
//! base64 so verifiers hash precisely the bytes Mint signed rather than
//! attempting to reproduce JSON serialization.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use mint_core::{op_hash, AccountId, HoldId};
use mint_receipts::{Receipt, ReceiptOp, Signer};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

const FORMAT: &str = "mint-transfer-attestation-v1";
const DIGEST_DOMAIN: &[u8] = b"mint-transfer-attestation-v1\0";
const REFERENCE_DOMAIN: &[u8] = b"mint-transfer-reference-v1\0";
const PARTY_DOMAIN: &[u8] = b"mint-transfer-party-v1\0";
const HOLD_FORMAT: &str = "mint-hold-settlement-attestation-v1";
const HOLD_DIGEST_DOMAIN: &[u8] = b"mint-hold-settlement-attestation-v1\0";
const HOLD_REFERENCE_DOMAIN: &[u8] = b"mint-hold-settlement-reference-v1\0";
const HOLD_ID_DOMAIN: &[u8] = b"mint-hold-settlement-hold-v1\0";
const HOLD_PARTY_DOMAIN: &[u8] = b"mint-hold-settlement-party-v1\0";

/// The ordered JSON claims whose exact bytes are signed.
///
/// Field declaration order is wire-significant. Consumers MUST decode
/// `payload_base64url` and verify those bytes directly; reserialization is not
/// part of verification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferAttestationClaims {
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
    pub sender_commitment: String,
    pub recipient_commitment: String,
    pub signer_key_id: String,
}

/// Signed transfer attestation returned by `POST /v1/transfers`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferAttestation {
    pub format: String,
    pub payload_base64url: String,
    pub digest_sha256: String,
    pub signer_key_id: String,
    pub signature_hex: String,
}

/// Privacy-safe claims for one terminal hold operation.
///
/// Field declaration order is wire-significant. Account ids, the hold id, and
/// the caller reference are represented only by domain-separated commitments.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

/// Independently verifiable evidence for a hold capture or release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldSettlementAttestation {
    pub format: String,
    pub payload_base64url: String,
    pub digest_sha256: String,
    pub signer_key_id: String,
    pub signature_hex: String,
}

/// Exact terminal hold command Mint is attesting.
#[derive(Clone, Copy, Debug)]
pub enum HoldSettlementOperation<'a> {
    Capture {
        payee: &'a AccountId,
        amount_micros: i64,
    },
    Release,
}

/// One active or historical verification key in the discovery document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationKeyDocument {
    pub key_id: String,
    pub algorithm: String,
    pub public_key_hex: String,
    pub status: String,
}

/// Rotation-shaped public verification-key document.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationKeysDocument {
    pub keys: Vec<VerificationKeyDocument>,
}

/// Verification failures are deliberately precise enough for clients to fail
/// closed without logging private transfer inputs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttestationVerifyError {
    Format,
    Encoding,
    Digest,
    KeyId,
    Signature,
    Claims,
}

/// Mint refuses to attest evidence that is not a valid receipt and command
/// under the same active key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttestationBuildError {
    Receipt,
    SignerMismatch,
    Parties,
    Intent,
}

impl TransferAttestation {
    /// Verify the exact transmitted bytes and return their decoded claims.
    pub fn verify(
        &self,
        expected_key_id: &str,
        key: &VerifyingKey,
    ) -> Result<TransferAttestationClaims, AttestationVerifyError> {
        if self.format != FORMAT {
            return Err(AttestationVerifyError::Format);
        }
        if self.signer_key_id != expected_key_id {
            return Err(AttestationVerifyError::KeyId);
        }

        let payload = URL_SAFE_NO_PAD
            .decode(&self.payload_base64url)
            .map_err(|_| AttestationVerifyError::Encoding)?;
        let digest = attestation_digest(&payload);
        if hex::encode(digest) != self.digest_sha256 {
            return Err(AttestationVerifyError::Digest);
        }
        let signature_bytes =
            hex::decode(&self.signature_hex).map_err(|_| AttestationVerifyError::Encoding)?;
        let signature_bytes: [u8; 64] = signature_bytes
            .try_into()
            .map_err(|_| AttestationVerifyError::Encoding)?;
        key.verify(&digest, &Signature::from_bytes(&signature_bytes))
            .map_err(|_| AttestationVerifyError::Signature)?;

        let claims: TransferAttestationClaims =
            serde_json::from_slice(&payload).map_err(|_| AttestationVerifyError::Claims)?;
        if claims.version != 1
            || claims.asset != "AIT"
            || claims.unit != "micro_usd"
            || claims.operation != "Transfer"
            || claims.signer_key_id != self.signer_key_id
        {
            return Err(AttestationVerifyError::Claims);
        }
        Ok(claims)
    }
}

impl HoldSettlementAttestation {
    /// Verify the exact transmitted payload bytes and return decoded claims.
    /// Callers must additionally compare the returned commitments and amount
    /// with their own persisted command before accepting the proof.
    pub fn verify(
        &self,
        expected_key_id: &str,
        key: &VerifyingKey,
    ) -> Result<HoldSettlementAttestationClaims, AttestationVerifyError> {
        if self.format != HOLD_FORMAT {
            return Err(AttestationVerifyError::Format);
        }
        if self.signer_key_id != expected_key_id {
            return Err(AttestationVerifyError::KeyId);
        }

        let payload = URL_SAFE_NO_PAD
            .decode(&self.payload_base64url)
            .map_err(|_| AttestationVerifyError::Encoding)?;
        let digest = hold_attestation_digest(&payload);
        if hex::encode(digest) != self.digest_sha256 {
            return Err(AttestationVerifyError::Digest);
        }
        let signature_bytes =
            hex::decode(&self.signature_hex).map_err(|_| AttestationVerifyError::Encoding)?;
        let signature_bytes: [u8; 64] = signature_bytes
            .try_into()
            .map_err(|_| AttestationVerifyError::Encoding)?;
        key.verify(&digest, &Signature::from_bytes(&signature_bytes))
            .map_err(|_| AttestationVerifyError::Signature)?;

        let claims: HoldSettlementAttestationClaims =
            serde_json::from_slice(&payload).map_err(|_| AttestationVerifyError::Claims)?;
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
            return Err(AttestationVerifyError::Claims);
        }
        Ok(claims)
    }
}

/// Build the signed attestation for the sender receipt emitted by one transfer.
pub fn build_transfer_attestation(
    signer: &dyn Signer,
    sender: &AccountId,
    recipient: &AccountId,
    reference: Option<&str>,
    receipt: &Receipt,
) -> Result<TransferAttestation, AttestationBuildError> {
    if receipt.body.signer != signer.key_id() {
        return Err(AttestationBuildError::SignerMismatch);
    }
    if receipt.body.op != ReceiptOp::Transfer
        || &receipt.body.account_id != sender
        || receipt.body.from.as_ref() != Some(sender)
        || receipt.body.to.as_ref() != Some(recipient)
    {
        return Err(AttestationBuildError::Parties);
    }
    if receipt.body.compute_hash() != receipt.hash {
        return Err(AttestationBuildError::Receipt);
    }
    let receipt_signature = Signature::from_bytes(receipt.sig.as_bytes());
    if signer
        .verifying_key()
        .verify(receipt.hash.as_bytes(), &receipt_signature)
        .is_err()
    {
        return Err(AttestationBuildError::Receipt);
    }
    let operation_hash = receipt.body.op_hash.to_hex();
    let signer_key_id = signer.key_id().as_str().to_owned();
    let claims = TransferAttestationClaims {
        version: 1,
        asset: "AIT".to_owned(),
        unit: "micro_usd".to_owned(),
        sequence: receipt.body.journal_seq,
        operation: "Transfer".to_owned(),
        operation_hash: operation_hash.clone(),
        amount_micros: receipt.body.amount_micros.get(),
        receipt_hash: receipt.hash.to_hex(),
        timestamp_ms: receipt.body.ts,
        reference_hash: reference_hash(reference.unwrap_or_default()),
        sender_commitment: party_commitment("sender", sender, &operation_hash),
        recipient_commitment: party_commitment("recipient", recipient, &operation_hash),
        signer_key_id: signer_key_id.clone(),
    };

    // Serialize the ordered struct directly. These are the authoritative bytes
    // included in the envelope, hashed, and signed.
    let payload = serde_json::to_vec(&claims).expect("attestation claims serialize");
    let digest = attestation_digest(&payload);
    let signature = signer.sign(&digest);

    Ok(TransferAttestation {
        format: FORMAT.to_owned(),
        payload_base64url: URL_SAFE_NO_PAD.encode(payload),
        digest_sha256: hex::encode(digest),
        signer_key_id,
        signature_hex: signature.to_hex(),
    })
}

/// Build a signed, privacy-safe attestation for one terminal hold operation.
///
/// Mint first verifies the complete ledger receipt, its signature, all parties,
/// the amount, and the canonical command hash. This prevents the compact legacy
/// `AITP-Receipt` fields from being substituted beside an unrelated valid
/// receipt hash/signature.
pub fn build_hold_settlement_attestation(
    signer: &dyn Signer,
    hold_id: &HoldId,
    payer: &AccountId,
    operation: HoldSettlementOperation<'_>,
    idem_key: &str,
    reference: Option<&str>,
    receipt: &Receipt,
) -> Result<HoldSettlementAttestation, AttestationBuildError> {
    if receipt.body.signer != signer.key_id() {
        return Err(AttestationBuildError::SignerMismatch);
    }
    if receipt.body.compute_hash() != receipt.hash {
        return Err(AttestationBuildError::Receipt);
    }
    let receipt_signature = Signature::from_bytes(receipt.sig.as_bytes());
    if signer
        .verifying_key()
        .verify(receipt.hash.as_bytes(), &receipt_signature)
        .is_err()
    {
        return Err(AttestationBuildError::Receipt);
    }

    let (operation_name, expected_hash, expected_amount, payee_commitment) = match operation {
        HoldSettlementOperation::Capture {
            payee,
            amount_micros,
        } => {
            if receipt.body.op != ReceiptOp::HoldCapture
                || &receipt.body.account_id != payer
                || receipt.body.from.as_ref() != Some(payer)
                || receipt.body.to.as_ref() != Some(payee)
            {
                return Err(AttestationBuildError::Parties);
            }
            let expected_hash = op_hash(&json!({
                "op": "hold.capture",
                "hold_id": hold_id.as_str(),
                "amount_micros": amount_micros,
                "to": payee.as_str(),
                "idem_key": idem_key,
                "ref": reference,
            }));
            (
                "hold.capture",
                expected_hash,
                amount_micros,
                hold_party_commitment("payee", payee, &receipt.body.op_hash.to_hex()),
            )
        }
        HoldSettlementOperation::Release => {
            if receipt.body.op != ReceiptOp::HoldRelease
                || &receipt.body.account_id != payer
                || receipt.body.from.as_ref() != Some(payer)
                || receipt.body.to.is_some()
            {
                return Err(AttestationBuildError::Parties);
            }
            let expected_hash = op_hash(&json!({
                "op": "hold.release",
                "hold_id": hold_id.as_str(),
                "idem_key": idem_key,
                "ref": reference,
            }));
            (
                "hold.release",
                expected_hash,
                receipt.body.amount_micros.get(),
                no_payee_commitment(&receipt.body.op_hash.to_hex()),
            )
        }
    };
    if receipt.body.op_hash != expected_hash
        || receipt.body.amount_micros.get() != expected_amount
        || expected_amount <= 0
    {
        return Err(AttestationBuildError::Intent);
    }

    let operation_hash = receipt.body.op_hash.to_hex();
    let signer_key_id = signer.key_id().as_str().to_owned();
    let claims = HoldSettlementAttestationClaims {
        version: 1,
        asset: "AIT".to_owned(),
        unit: "micro_usd".to_owned(),
        sequence: receipt.body.journal_seq,
        operation: operation_name.to_owned(),
        operation_hash: operation_hash.clone(),
        amount_micros: expected_amount,
        receipt_hash: receipt.hash.to_hex(),
        timestamp_ms: receipt.body.ts,
        reference_hash: hold_reference_hash(reference.unwrap_or_default()),
        hold_commitment: hold_commitment(hold_id, &operation_hash),
        payer_commitment: hold_party_commitment("payer", payer, &operation_hash),
        payee_commitment,
        signer_key_id: signer_key_id.clone(),
    };
    let payload = serde_json::to_vec(&claims).expect("hold settlement claims serialize");
    let digest = hold_attestation_digest(&payload);
    let signature = signer.sign(&digest);
    Ok(HoldSettlementAttestation {
        format: HOLD_FORMAT.to_owned(),
        payload_base64url: URL_SAFE_NO_PAD.encode(payload),
        digest_sha256: hex::encode(digest),
        signer_key_id,
        signature_hex: signature.to_hex(),
    })
}

/// Render the active receipt/attestation signer for independent discovery.
pub fn verification_keys_document(signer: &dyn Signer) -> VerificationKeysDocument {
    VerificationKeysDocument {
        keys: vec![VerificationKeyDocument {
            key_id: signer.key_id().as_str().to_owned(),
            algorithm: "Ed25519".to_owned(),
            public_key_hex: hex::encode(signer.verifying_key().as_bytes()),
            status: "active".to_owned(),
        }],
    }
}

fn reference_hash(reference: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(REFERENCE_DOMAIN);
    hash.update(reference.as_bytes());
    hex::encode(hash.finalize())
}

fn party_commitment(role: &str, account: &AccountId, operation_hash: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(PARTY_DOMAIN);
    hash.update(role.as_bytes());
    hash.update(b"\0");
    hash.update(account.as_str().as_bytes());
    hash.update(b"\0");
    hash.update(operation_hash.as_bytes());
    hex::encode(hash.finalize())
}

fn hold_reference_hash(reference: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(HOLD_REFERENCE_DOMAIN);
    hash.update(reference.as_bytes());
    hex::encode(hash.finalize())
}

fn hold_commitment(hold_id: &HoldId, operation_hash: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(HOLD_ID_DOMAIN);
    hash.update(hold_id.as_str().as_bytes());
    hash.update(b"\0");
    hash.update(operation_hash.as_bytes());
    hex::encode(hash.finalize())
}

fn hold_party_commitment(role: &str, account: &AccountId, operation_hash: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(HOLD_PARTY_DOMAIN);
    hash.update(role.as_bytes());
    hash.update(b"\0");
    hash.update(account.as_str().as_bytes());
    hash.update(b"\0");
    hash.update(operation_hash.as_bytes());
    hex::encode(hash.finalize())
}

fn no_payee_commitment(operation_hash: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(HOLD_PARTY_DOMAIN);
    hash.update(b"no-payee\0");
    hash.update(operation_hash.as_bytes());
    hex::encode(hash.finalize())
}

fn attestation_digest(payload: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(DIGEST_DOMAIN);
    hash.update(payload);
    hash.finalize().into()
}

fn hold_attestation_digest(payload: &[u8]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(HOLD_DIGEST_DOMAIN);
    hash.update(payload);
    hash.finalize().into()
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
    use mint_core::{AccountKind, Micros, OpHash};
    use mint_receipts::{FixedSigner, ReceiptBody, ReceiptHash, ReceiptOp, SignerKeyId};
    use ulid::Ulid;

    fn fixture() -> (FixedSigner, AccountId, AccountId, Receipt) {
        let signer = FixedSigner::new();
        let sender = AccountId::new(AccountKind::User, Ulid::from(1u128));
        let recipient = AccountId::new(AccountKind::Model, Ulid::from(2u128));
        let body = ReceiptBody {
            account_id: sender.clone(),
            account_seq: 7,
            journal_seq: 42,
            op: ReceiptOp::Transfer,
            op_hash: op_hash(&serde_json::json!({
                "op": "transfer",
                "from": sender.as_str(),
                "to": recipient.as_str(),
                "amount_micros": 4_200_000,
                "idem_key": "menivor:transfer:01",
                "ref": "menivor:transfer:01",
            })),
            amount_micros: Micros::new(4_200_000),
            from: Some(sender.clone()),
            to: Some(recipient.clone()),
            balance_after_available_micros: Micros::new(5_800_000),
            balance_after_held_micros: Micros::new(0),
            ts: 1_700_000_000_123,
            prev_hash: ReceiptHash::from_bytes([0x22; 32]),
            signer: SignerKeyId::new("mint-verifier-key-1"),
        };
        let receipt = Receipt {
            hash: body.compute_hash(),
            sig: signer.sign(body.compute_hash().as_bytes()),
            body,
        };
        (signer, sender, recipient, receipt)
    }

    fn hold_fixture(capture: bool) -> (FixedSigner, HoldId, AccountId, AccountId, String, Receipt) {
        let signer = FixedSigner::new();
        let hold_id = HoldId::new(Ulid::from(3u128));
        let payer = AccountId::new(AccountKind::User, Ulid::from(4u128));
        let payee = AccountId::new(AccountKind::Model, Ulid::from(5u128));
        let reference = "menivor:generation:job_01".to_owned();
        let (op, operation_hash, amount, to) = if capture {
            (
                ReceiptOp::HoldCapture,
                op_hash(&json!({
                    "op": "hold.capture",
                    "hold_id": hold_id.as_str(),
                    "amount_micros": 2_500_000,
                    "to": payee.as_str(),
                    "idem_key": "job_01:capture",
                    "ref": reference,
                })),
                2_500_000,
                Some(payee.clone()),
            )
        } else {
            (
                ReceiptOp::HoldRelease,
                op_hash(&json!({
                    "op": "hold.release",
                    "hold_id": hold_id.as_str(),
                    "idem_key": "job_01:release",
                    "ref": reference,
                })),
                4_000_000,
                None,
            )
        };
        let body = ReceiptBody {
            account_id: payer.clone(),
            account_seq: 8,
            journal_seq: 52,
            op,
            op_hash: operation_hash,
            amount_micros: Micros::new(amount),
            from: Some(payer.clone()),
            to,
            balance_after_available_micros: Micros::new(6_000_000),
            balance_after_held_micros: Micros::new(0),
            ts: 1_700_000_000_456,
            prev_hash: ReceiptHash::from_bytes([0x44; 32]),
            signer: SignerKeyId::new("mint-verifier-key-1"),
        };
        let receipt = Receipt {
            hash: body.compute_hash(),
            sig: signer.sign(body.compute_hash().as_bytes()),
            body,
        };
        (signer, hold_id, payer, payee, reference, receipt)
    }

    #[test]
    fn exact_bytes_verify_and_tampering_fails() {
        let (signer, sender, recipient, receipt) = fixture();
        let evidence = build_transfer_attestation(
            &signer,
            &sender,
            &recipient,
            Some("menivor:transfer:01"),
            &receipt,
        )
        .unwrap();
        let claims = evidence
            .verify(signer.key_id().as_str(), &signer.verifying_key())
            .unwrap();
        assert_eq!(claims.sequence, 42);
        assert_eq!(claims.amount_micros, 4_200_000);

        let mut tampered = evidence.clone();
        tampered.payload_base64url.push('A');
        assert!(matches!(
            tampered.verify(signer.key_id().as_str(), &signer.verifying_key()),
            Err(AttestationVerifyError::Digest | AttestationVerifyError::Encoding)
        ));
    }

    #[test]
    fn key_id_and_key_mismatch_fail_closed() {
        let (signer, sender, recipient, receipt) = fixture();
        let evidence =
            build_transfer_attestation(&signer, &sender, &recipient, None, &receipt).unwrap();
        assert_eq!(
            evidence.verify("wrong-key-id", &signer.verifying_key()),
            Err(AttestationVerifyError::KeyId)
        );

        let other = mint_receipts::Ed25519Signer::from_seed([0x77; 32], "other");
        assert_eq!(
            evidence.verify(signer.key_id().as_str(), &other.verifying_key()),
            Err(AttestationVerifyError::Signature)
        );
        assert_eq!(
            build_transfer_attestation(&other, &sender, &recipient, None, &receipt),
            Err(AttestationBuildError::SignerMismatch),
            "Mint must not attest a receipt emitted under another signer"
        );

        let mut corrupt_receipt = receipt.clone();
        corrupt_receipt.hash = ReceiptHash::from_bytes([0x99; 32]);
        assert_eq!(
            build_transfer_attestation(&signer, &sender, &recipient, None, &corrupt_receipt,),
            Err(AttestationBuildError::Receipt),
            "Mint must not attest a corrupt source receipt"
        );
    }

    #[test]
    fn claims_are_private_and_commitments_are_per_transfer() {
        let (signer, sender, recipient, receipt) = fixture();
        let evidence =
            build_transfer_attestation(&signer, &sender, &recipient, None, &receipt).unwrap();
        let payload = URL_SAFE_NO_PAD.decode(&evidence.payload_base64url).unwrap();
        let payload = String::from_utf8(payload).unwrap();
        assert!(!payload.contains(sender.as_str()));
        assert!(!payload.contains(recipient.as_str()));

        let mut second_receipt = receipt.clone();
        second_receipt.body.op_hash = OpHash::from_bytes([0x33; 32]);
        second_receipt.hash = second_receipt.body.compute_hash();
        second_receipt.sig = signer.sign(second_receipt.hash.as_bytes());
        let second =
            build_transfer_attestation(&signer, &sender, &recipient, None, &second_receipt)
                .unwrap();
        let first_claims = evidence
            .verify(signer.key_id().as_str(), &signer.verifying_key())
            .unwrap();
        let second_claims = second
            .verify(signer.key_id().as_str(), &signer.verifying_key())
            .unwrap();
        assert_ne!(
            first_claims.sender_commitment,
            second_claims.sender_commitment
        );
        assert_ne!(
            first_claims.recipient_commitment,
            second_claims.recipient_commitment
        );
    }

    #[test]
    fn golden_vector_is_stable() {
        let (signer, sender, recipient, receipt) = fixture();
        let evidence = build_transfer_attestation(
            &signer,
            &sender,
            &recipient,
            Some("menivor:transfer:01"),
            &receipt,
        )
        .unwrap();
        assert_eq!(sender.as_str(), "acct_000000000000000000000000001");
        assert_eq!(recipient.as_str(), "acct_400000000000000000000000002");
        assert_eq!(evidence.format, FORMAT);
        assert_eq!(
            hex::encode(signer.verifying_key().as_bytes()),
            "74e0be0d9692f0817482d8f3930b51d334db23d11665ca8ee52fe9e8c29715e8"
        );
        assert_eq!(
            evidence.payload_base64url,
            "eyJ2ZXJzaW9uIjoxLCJhc3NldCI6IkFJVCIsInVuaXQiOiJtaWNyb191c2QiLCJzZXF1ZW5jZSI6NDIsIm9wZXJhdGlvbiI6IlRyYW5zZmVyIiwib3BlcmF0aW9uX2hhc2giOiI0ODFjYmNiMTk3MjE1MTM5Y2E1MTU3YmE5MTJmMjUyZWEwYjI5NDNlNDQxY2E3MjhmOGJiZWExNzlhZjEyZWNlIiwiYW1vdW50X21pY3JvcyI6NDIwMDAwMCwicmVjZWlwdF9oYXNoIjoiYmNlZTdmNTI2NDlkYWIwZjU4Nzk1N2EyODNmOGNmMTg2MDgwNjI4MjgzNGFlNjNkZTc2MmUzZmNiMjY5NjE5NyIsInRpbWVzdGFtcF9tcyI6MTcwMDAwMDAwMDEyMywicmVmZXJlbmNlX2hhc2giOiIzNDgyZWE2MjVlNGYzYzkwOGYwMTI5ZTAwYjYzYzg3N2E0ZTAxMGExMmE4OTE5MzFiMTI1NDE0YTEwYWI1MDQxIiwic2VuZGVyX2NvbW1pdG1lbnQiOiIxNDU1ZmY1NzhhZTYwM2Y1YzlhYWU4MWYxMzdhZWY0NDFlNjdkNGRjNjA5MjAyMGNiZDA3OWRhZmM5ZDU2N2Y1IiwicmVjaXBpZW50X2NvbW1pdG1lbnQiOiI3MjYwYTAxNDkyNTRmOWJjZjYxZDA0MWU5OGJhMGZjNTA3YWQ5NDZhMGVkY2Q0MGZmOGQ0Y2ZkOGZiY2ExOTc3Iiwic2lnbmVyX2tleV9pZCI6Im1pbnQtdmVyaWZpZXIta2V5LTEifQ"
        );
        assert_eq!(
            evidence.digest_sha256,
            "a365e20ff04403411ff2abc324fbfbba4cd8839f0f620fae9d6eae6c56848355"
        );
        assert_eq!(
            evidence.signature_hex,
            "f8f7af24c68eca99bd5250e7aa44c8b0d4d617f44df8c5b1d274d1e5d4e18cc946308cd259134d8381d9b7357caee00c2566f19edb954876c303813216330c09"
        );
    }

    #[test]
    fn hold_capture_attestation_binds_command_and_is_private() {
        let (signer, hold_id, payer, payee, reference, receipt) = hold_fixture(true);
        let evidence = build_hold_settlement_attestation(
            &signer,
            &hold_id,
            &payer,
            HoldSettlementOperation::Capture {
                payee: &payee,
                amount_micros: 2_500_000,
            },
            "job_01:capture",
            Some(&reference),
            &receipt,
        )
        .unwrap();
        let claims = evidence
            .verify(signer.key_id().as_str(), &signer.verifying_key())
            .unwrap();
        assert_eq!(evidence.format, HOLD_FORMAT);
        assert_eq!(
            evidence.payload_base64url,
            "eyJ2ZXJzaW9uIjoxLCJhc3NldCI6IkFJVCIsInVuaXQiOiJtaWNyb191c2QiLCJzZXF1ZW5jZSI6NTIsIm9wZXJhdGlvbiI6ImhvbGQuY2FwdHVyZSIsIm9wZXJhdGlvbl9oYXNoIjoiZWFmNjA5NmVhNzg1MDcyNmJkN2NlZmI3NjQ3ZDM2MGRjYmE2N2IwOTk1OGUyZjNkMDJmZjJmN2QyODQ3N2Q3NyIsImFtb3VudF9taWNyb3MiOjI1MDAwMDAsInJlY2VpcHRfaGFzaCI6ImQyMDlmYjQzOTk0MTZiMWE2NzdkNmM3MDdjN2ZmYTliNWI4Zjg4MGEzNzgzYzQ3MTIyMWUzYzI3MWVlMWVjMDciLCJ0aW1lc3RhbXBfbXMiOjE3MDAwMDAwMDA0NTYsInJlZmVyZW5jZV9oYXNoIjoiOTBlMjNmMzhmMWUxNGJjOTk4ZDMyZWZjNjJlMjljOGQ4NjE3YmExM2I3OTRkNjRhNjAyZDU1NWEzY2ZkZjVkNyIsImhvbGRfY29tbWl0bWVudCI6Ijg5MzkyOTI4NWFkZmNiNGFkMDgwMTdjMWU2ZDc3MjY1MjhlNGMxZWVjZTRlN2EyNTE2NWEwMWQxMjg5MWRlZDEiLCJwYXllcl9jb21taXRtZW50IjoiMmQxNjQ2YmU2NDJmNjA3YzNjMWNkNjczMDg5N2IwODRmMTExMDg0ZWRhNDFkYjFlNzkzNzk2YzA0YTMyNDdjYyIsInBheWVlX2NvbW1pdG1lbnQiOiJjZTliMDNiOTBmZGIzYWM5ODM3NDEyNzUyOWJjOWU0MmYzOThhN2NhNzY0MTk2OTkyNTgxOWQ2NTMxY2EwNmEzIiwic2lnbmVyX2tleV9pZCI6Im1pbnQtdmVyaWZpZXIta2V5LTEifQ"
        );
        assert_eq!(
            evidence.digest_sha256,
            "ac410b86bdff39fbe42fe90ae5a3a33747bd285c2eb891bedad59ffa692be898"
        );
        assert_eq!(
            evidence.signature_hex,
            "4411eadc0d98ac00b792a2029e43ad80458538ab2c7f1306417f92459b9ac903a982a5628f31fe49b3ea8dbc5b5de2b8777b1b7c22a2edf2280f52193cc2d500"
        );
        assert_eq!(claims.operation, "hold.capture");
        assert_eq!(claims.amount_micros, 2_500_000);
        assert_eq!(claims.operation_hash, receipt.body.op_hash.to_hex());
        assert_eq!(claims.receipt_hash, receipt.hash.to_hex());
        assert_eq!(claims.timestamp_ms, receipt.body.ts);
        assert_eq!(claims.reference_hash, hold_reference_hash(&reference));
        assert_eq!(
            claims.hold_commitment,
            hold_commitment(&hold_id, &claims.operation_hash)
        );
        assert_eq!(
            claims.payer_commitment,
            hold_party_commitment("payer", &payer, &claims.operation_hash)
        );
        assert_eq!(
            claims.payee_commitment,
            hold_party_commitment("payee", &payee, &claims.operation_hash)
        );

        let payload = URL_SAFE_NO_PAD.decode(&evidence.payload_base64url).unwrap();
        let payload = String::from_utf8(payload).unwrap();
        assert!(!payload.contains(hold_id.as_str()));
        assert!(!payload.contains(payer.as_str()));
        assert!(!payload.contains(payee.as_str()));
        assert!(!payload.contains(&reference));
    }

    #[test]
    fn hold_attestation_rejects_amount_reference_party_and_operation_drift() {
        let (signer, hold_id, payer, payee, reference, receipt) = hold_fixture(true);
        let build = |operation, reference: Option<&str>, receipt: &Receipt| {
            build_hold_settlement_attestation(
                &signer,
                &hold_id,
                &payer,
                operation,
                "job_01:capture",
                reference,
                receipt,
            )
        };

        assert_eq!(
            build(
                HoldSettlementOperation::Capture {
                    payee: &payee,
                    amount_micros: 2_400_000,
                },
                Some(&reference),
                &receipt,
            ),
            Err(AttestationBuildError::Intent)
        );
        assert_eq!(
            build(
                HoldSettlementOperation::Capture {
                    payee: &payee,
                    amount_micros: 2_500_000,
                },
                Some("another-job"),
                &receipt,
            ),
            Err(AttestationBuildError::Intent)
        );

        let other_payee = AccountId::new(AccountKind::Model, Ulid::from(6u128));
        assert_eq!(
            build(
                HoldSettlementOperation::Capture {
                    payee: &other_payee,
                    amount_micros: 2_500_000,
                },
                Some(&reference),
                &receipt,
            ),
            Err(AttestationBuildError::Parties)
        );
        assert_eq!(
            build(HoldSettlementOperation::Release, Some(&reference), &receipt,),
            Err(AttestationBuildError::Parties)
        );
    }

    #[test]
    fn hold_attestation_tampering_fails_and_replay_is_byte_identical() {
        let (signer, hold_id, payer, payee, reference, receipt) = hold_fixture(true);
        let build = || {
            build_hold_settlement_attestation(
                &signer,
                &hold_id,
                &payer,
                HoldSettlementOperation::Capture {
                    payee: &payee,
                    amount_micros: 2_500_000,
                },
                "job_01:capture",
                Some(&reference),
                &receipt,
            )
            .unwrap()
        };
        let first = build();
        assert_eq!(
            serde_json::to_vec(&first).unwrap(),
            serde_json::to_vec(&build()).unwrap()
        );

        let mut payload: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(&first.payload_base64url).unwrap())
                .unwrap();
        payload["amount_micros"] = json!(1);
        let mut tampered = first;
        tampered.payload_base64url = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
        assert_eq!(
            tampered.verify(signer.key_id().as_str(), &signer.verifying_key()),
            Err(AttestationVerifyError::Digest)
        );
    }

    #[test]
    fn hold_release_attestation_binds_explicit_no_payee() {
        let (signer, hold_id, payer, _payee, reference, receipt) = hold_fixture(false);
        let evidence = build_hold_settlement_attestation(
            &signer,
            &hold_id,
            &payer,
            HoldSettlementOperation::Release,
            "job_01:release",
            Some(&reference),
            &receipt,
        )
        .unwrap();
        let claims = evidence
            .verify(signer.key_id().as_str(), &signer.verifying_key())
            .unwrap();
        assert_eq!(claims.operation, "hold.release");
        assert_eq!(claims.amount_micros, 4_000_000);
        assert_eq!(
            claims.payee_commitment,
            no_payee_commitment(&claims.operation_hash)
        );
    }
}
