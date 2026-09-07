//! Public key discovery for offline receipt and attestation verification.

use serde::Deserialize;

use crate::{Error, MintClient};

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct VerificationKey {
    pub key_id: String,
    pub algorithm: String,
    pub public_key_hex: String,
    pub status: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct VerificationKeys {
    pub keys: Vec<VerificationKey>,
}

impl MintClient {
    pub async fn verification_keys(&self) -> Result<VerificationKeys, Error> {
        self.get(".well-known/mint-verification-keys.json").await
    }
}
