//! Signing seam: the [`Signer`] trait (Ed25519 over a 32-byte receipt hash), an
//! [`Ed25519Signer`] impl, a deterministic [`FixedSigner`] for tests, and a
//! [`Keyring`] the offline verifier uses to check signatures with public keys
//! only (no private key needed to verify).

use std::collections::HashMap;

use ed25519_dalek::{Signer as _, SigningKey, VerifyingKey};

use crate::receipt::{ReceiptSig, SignerKeyId};

/// The default deterministic verifier key id used by the store's default
/// constructors. A constant id keeps hashes reproducible across runs.
pub const DEFAULT_KEY_ID: &str = "mint-verifier-key-1";

/// A fixed 32-byte seed for [`FixedSigner`] and the store's default signer, so
/// signatures (not just hashes) are byte-reproducible in tests.
pub const FIXED_SEED: [u8; 32] = [
    0x9e, 0x3f, 0x2c, 0x11, 0xaa, 0xbb, 0xcc, 0xdd, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08,
    0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xa0, 0xb0, 0xc0, 0xd0, 0xe0, 0xf0, 0x10,
];

/// Signs receipt hashes and exposes the matching verifying key. `Send + Sync` so
/// it can live behind a shared store.
pub trait Signer: Send + Sync {
    /// The id of this signer's key (stamped into every receipt body).
    fn key_id(&self) -> SignerKeyId;

    /// Sign a 32-byte receipt hash, producing a 64-byte Ed25519 signature.
    fn sign(&self, msg: &[u8; 32]) -> ReceiptSig;

    /// The public verifying key (registered in a [`Keyring`] for offline verify).
    fn verifying_key(&self) -> VerifyingKey;
}

/// A concrete Ed25519 signer holding a private [`SigningKey`] and its id.
pub struct Ed25519Signer {
    key: SigningKey,
    id: SignerKeyId,
}

impl Ed25519Signer {
    /// Build from a raw 32-byte seed (deterministic — same seed ⇒ same key).
    pub fn from_seed(seed: [u8; 32], id: impl Into<String>) -> Self {
        Ed25519Signer {
            key: SigningKey::from_bytes(&seed),
            id: SignerKeyId::new(id),
        }
    }

    /// Generate a fresh random key from OS entropy (production key creation; not
    /// used by the deterministic test path).
    pub fn generate(id: impl Into<String>) -> Self {
        let mut seed = [0u8; 32];
        getrandom::getrandom(&mut seed).expect("OS randomness available");
        Ed25519Signer::from_seed(seed, id)
    }
}

impl Signer for Ed25519Signer {
    fn key_id(&self) -> SignerKeyId {
        self.id.clone()
    }

    fn sign(&self, msg: &[u8; 32]) -> ReceiptSig {
        let sig = self.key.sign(msg);
        ReceiptSig::from_bytes(sig.to_bytes())
    }

    fn verifying_key(&self) -> VerifyingKey {
        self.key.verifying_key()
    }
}

/// A deterministic test signer: [`FIXED_SEED`] + [`DEFAULT_KEY_ID`], so both the
/// hash AND the signature are byte-reproducible.
pub struct FixedSigner(Ed25519Signer);

impl Default for FixedSigner {
    fn default() -> Self {
        FixedSigner::new()
    }
}

impl FixedSigner {
    /// The fixed deterministic signer (`FIXED_SEED`, `DEFAULT_KEY_ID`).
    pub fn new() -> Self {
        FixedSigner(Ed25519Signer::from_seed(FIXED_SEED, DEFAULT_KEY_ID))
    }
}

impl Signer for FixedSigner {
    fn key_id(&self) -> SignerKeyId {
        self.0.key_id()
    }

    fn sign(&self, msg: &[u8; 32]) -> ReceiptSig {
        self.0.sign(msg)
    }

    fn verifying_key(&self) -> VerifyingKey {
        self.0.verifying_key()
    }
}

/// A registry of public verifying keys, keyed by [`SignerKeyId`]. The offline
/// verifier needs only this (no private keys) to check every receipt signature.
#[derive(Default, Clone)]
pub struct Keyring {
    keys: HashMap<SignerKeyId, VerifyingKey>,
}

impl Keyring {
    /// An empty keyring.
    pub fn new() -> Self {
        Keyring {
            keys: HashMap::new(),
        }
    }

    /// A keyring seeded with one signer's public key (the common test case).
    pub fn with_signer(signer: &dyn Signer) -> Self {
        let mut kr = Keyring::new();
        kr.insert(signer.key_id(), signer.verifying_key());
        kr
    }

    /// Register (or overwrite) a public key under its id.
    pub fn insert(&mut self, id: SignerKeyId, key: VerifyingKey) {
        self.keys.insert(id, key);
    }

    /// Look up a public key by id.
    pub fn get(&self, id: &SignerKeyId) -> Option<&VerifyingKey> {
        self.keys.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier};

    #[test]
    fn fixed_signer_is_deterministic() {
        let a = FixedSigner::new();
        let b = FixedSigner::new();
        let msg = [42u8; 32];
        assert_eq!(a.sign(&msg), b.sign(&msg));
        assert_eq!(a.key_id(), SignerKeyId::new(DEFAULT_KEY_ID));
    }

    #[test]
    fn signature_verifies_with_public_key() {
        let s = FixedSigner::new();
        let msg = [7u8; 32];
        let sig = s.sign(&msg);
        let vk = s.verifying_key();
        let parsed = Signature::from_bytes(sig.as_bytes());
        assert!(vk.verify(&msg, &parsed).is_ok());
        // A different message must NOT verify.
        assert!(vk.verify(&[8u8; 32], &parsed).is_err());
    }

    #[test]
    fn keyring_round_trips_a_signer() {
        let s = FixedSigner::new();
        let kr = Keyring::with_signer(&s);
        assert!(kr.get(&s.key_id()).is_some());
        assert!(kr.get(&SignerKeyId::new("missing")).is_none());
    }

    #[test]
    fn generate_produces_a_usable_key() {
        let s = Ed25519Signer::generate("k-gen");
        let msg = [1u8; 32];
        let sig = s.sign(&msg);
        let parsed = Signature::from_bytes(sig.as_bytes());
        assert!(s.verifying_key().verify(&msg, &parsed).is_ok());
    }
}
