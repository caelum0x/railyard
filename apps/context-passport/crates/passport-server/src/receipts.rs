//! Cryptographic verification of Mint settlement receipts.
//!
//! Mint signs every receipt with Ed25519 over the 32 raw bytes of the receipt
//! hash (see `mint-receipts`), and publishes its public keys at
//! `GET /.well-known/mint-verification-keys.json`. Identity Lock refuses to
//! record provenance on the strength of settlement evidence it cannot verify:
//! a fabricated `signature: "x"` or a receipt signed by any key other than
//! Mint's is rejected before a GenerationRecord or C2PA manifest exists.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use passport_core::PaymentEvidence;

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
            .verify(&evidence(&hex::encode(hash), &hex::encode(other.to_bytes())))
            .is_err());
    }
}
