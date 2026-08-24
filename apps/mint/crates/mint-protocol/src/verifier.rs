//! Per-payer verifying-key seam (SPEC §3 verify step).
//!
//! The ledger store has no per-account public-key registry, so AP3 introduces
//! this [`PayerKeys`] seam: given a `payer` account id and the `pubkey_id`
//! stamped on an authorization, resolve the Ed25519 [`VerifyingKey`] that must
//! validate its signature. This is the SECOND keyspace in the system — distinct
//! from the mint-verifier key that signs §6 receipts (the payer signs the
//! authorization; the server signs the receipt).

use std::collections::HashMap;

use ed25519_dalek::VerifyingKey;
use mint_receipts::Signer;

/// Resolve the verifying key for a `(payer, pubkey_id)` pair. `Send + Sync` so it
/// can live behind the shared protocol state machine.
pub trait PayerKeys: Send + Sync {
    /// The registered public key for `payer`'s `pubkey_id`, or `None` if unknown
    /// (an unknown key is [`mint_core::MintError::SignatureInvalid`] upstream).
    fn verifying_key(&self, payer: &str, pubkey_id: &str) -> Option<VerifyingKey>;
}

/// An in-memory [`PayerKeys`] registry keyed by `(payer, pubkey_id)`. Built in
/// tests from a `mint_receipts` signer's public key.
#[derive(Default, Clone)]
pub struct InMemoryPayerKeys {
    keys: HashMap<(String, String), VerifyingKey>,
}

impl InMemoryPayerKeys {
    /// An empty registry.
    pub fn new() -> Self {
        InMemoryPayerKeys {
            keys: HashMap::new(),
        }
    }

    /// Register (or overwrite) a payer's public key under a `pubkey_id`.
    pub fn insert(
        &mut self,
        payer: impl Into<String>,
        pubkey_id: impl Into<String>,
        key: VerifyingKey,
    ) {
        self.keys.insert((payer.into(), pubkey_id.into()), key);
    }

    /// Register a payer key straight from a signer, using the signer's own
    /// `key_id()` as the `pubkey_id` (the common test path — the same signer is
    /// passed to [`crate::intent::build_authorization`]).
    pub fn with_payer_signer(payer: impl Into<String>, signer: &dyn Signer) -> Self {
        let mut k = InMemoryPayerKeys::new();
        k.insert(
            payer,
            signer.key_id().as_str().to_string(),
            signer.verifying_key(),
        );
        k
    }
}

impl PayerKeys for InMemoryPayerKeys {
    fn verifying_key(&self, payer: &str, pubkey_id: &str) -> Option<VerifyingKey> {
        self.keys
            .get(&(payer.to_string(), pubkey_id.to_string()))
            .copied()
    }
}
