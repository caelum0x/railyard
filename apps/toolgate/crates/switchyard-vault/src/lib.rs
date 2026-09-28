//! # switchyard-vault
//!
//! Envelope encryption for stored provider credentials.
//!
//! - Each org has a random 256-bit **data key** (DEK). Credentials are sealed
//!   with XChaCha20-Poly1305 under the DEK, a random 24-byte nonce, and the
//!   associated data `org_id|credential_id`, so a ciphertext copied to another
//!   row or org fails to open.
//! - The DEK is stored only **wrapped** by a key-encryption key (KEK) from
//!   `SWITCHYARD_KEK` (32 bytes, base64), with associated data `dek|org_id`.
//! - Every wrapped DEK records its `kek_version`. Old KEKs stay loadable
//!   (`SWITCHYARD_KEK_PREVIOUS=2:<b64>,1:<b64>`) so rotation is a background
//!   re-wrap (`switchyard-server rotate-kek`), never downtime. Losing one KEK
//!   loses nothing once rows are re-wrapped under the current version.
//! - Plaintext keys and secrets live in zeroising containers.

use std::collections::BTreeMap;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use secrecy::SecretString;
use zeroize::Zeroizing;

pub const NONCE_LEN: usize = 24;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum VaultError {
    #[error("invalid key material: {0}")]
    InvalidKey(String),
    #[error("unknown KEK version {0}")]
    UnknownKekVersion(u32),
    #[error("decryption failed (wrong key, wrong associated data, or tampered ciphertext)")]
    Decrypt,
    #[error("encryption failed")]
    Encrypt,
}

/// A DEK wrapped under a KEK version: `nonce || ciphertext`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedDek {
    pub wrapped: Vec<u8>,
    pub kek_version: u32,
}

/// A sealed credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub ciphertext: Vec<u8>,
    pub nonce: Vec<u8>,
}

pub struct Vault {
    keks: BTreeMap<u32, Zeroizing<[u8; 32]>>,
    current: u32,
}

impl std::fmt::Debug for Vault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vault").field("current", &self.current).field("versions", &self.keks.keys().collect::<Vec<_>>()).finish()
    }
}

fn decode_key(b64: &str) -> Result<Zeroizing<[u8; 32]>, VaultError> {
    let raw = Zeroizing::new(B64.decode(b64.trim()).map_err(|e| VaultError::InvalidKey(e.to_string()))?);
    if raw.len() != 32 {
        return Err(VaultError::InvalidKey(format!("expected 32 bytes, got {}", raw.len())));
    }
    let mut k = Zeroizing::new([0u8; 32]);
    k.copy_from_slice(&raw);
    Ok(k)
}

fn aad(parts: &[&str]) -> Vec<u8> {
    parts.join("|").into_bytes()
}

impl Vault {
    /// A vault with one current KEK.
    pub fn new(current_version: u32, kek_b64: &str) -> Result<Self, VaultError> {
        let mut keks = BTreeMap::new();
        keks.insert(current_version, decode_key(kek_b64)?);
        Ok(Vault { keks, current: current_version })
    }

    /// Add a previous KEK so rows wrapped under it can still be opened and re-wrapped.
    pub fn with_previous(mut self, version: u32, kek_b64: &str) -> Result<Self, VaultError> {
        if version == self.current {
            return Err(VaultError::InvalidKey("previous KEK version equals current".into()));
        }
        self.keks.insert(version, decode_key(kek_b64)?);
        Ok(self)
    }

    /// Parse `SWITCHYARD_KEK`, `SWITCHYARD_KEK_VERSION` and `SWITCHYARD_KEK_PREVIOUS`.
    pub fn from_env_values(kek: &str, version: Option<&str>, previous: Option<&str>) -> Result<Self, VaultError> {
        let v: u32 = version.unwrap_or("1").parse().map_err(|_| VaultError::InvalidKey("SWITCHYARD_KEK_VERSION".into()))?;
        let mut vault = Vault::new(v, kek)?;
        for entry in previous.unwrap_or("").split(',').filter(|s| !s.trim().is_empty()) {
            let (ver, key) = entry.split_once(':').ok_or_else(|| VaultError::InvalidKey("SWITCHYARD_KEK_PREVIOUS format is v:b64".into()))?;
            let ver: u32 = ver.trim().parse().map_err(|_| VaultError::InvalidKey("previous KEK version".into()))?;
            vault = vault.with_previous(ver, key)?;
        }
        Ok(vault)
    }

    /// A fresh random KEK, base64 (for `switchyard-server gen-kek`).
    pub fn generate_kek_b64() -> String {
        let key = XChaCha20Poly1305::generate_key(&mut OsRng);
        B64.encode(key)
    }

    pub fn current_version(&self) -> u32 {
        self.current
    }

    fn cipher(key: &[u8; 32]) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(key.into())
    }

    fn seal_raw(key: &[u8; 32], plaintext: &[u8], aad: &[u8]) -> Result<(Vec<u8>, Vec<u8>), VaultError> {
        let nonce = XChaCha20Poly1305::generate_nonce(&mut OsRng);
        let ct = Self::cipher(key).encrypt(&nonce, Payload { msg: plaintext, aad }).map_err(|_| VaultError::Encrypt)?;
        Ok((ct, nonce.to_vec()))
    }

    fn open_raw(key: &[u8; 32], ct: &[u8], nonce: &[u8], aad: &[u8]) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        if nonce.len() != NONCE_LEN {
            return Err(VaultError::Decrypt);
        }
        Self::cipher(key)
            .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
            .map(Zeroizing::new)
            .map_err(|_| VaultError::Decrypt)
    }

    /// Create a new org DEK, returned wrapped under the current KEK.
    pub fn new_org_dek(&self, org_id: &str) -> Result<WrappedDek, VaultError> {
        let mut raw = Zeroizing::new([0u8; 32]);
        rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, raw.as_mut());
        self.wrap(&raw, org_id)
    }

    fn wrap(&self, dek: &[u8; 32], org_id: &str) -> Result<WrappedDek, VaultError> {
        let kek = &self.keks[&self.current];
        let (ct, nonce) = Self::seal_raw(kek, dek, &aad(&["dek", org_id]))?;
        let mut wrapped = nonce;
        wrapped.extend_from_slice(&ct);
        Ok(WrappedDek { wrapped, kek_version: self.current })
    }

    fn unwrap(&self, w: &WrappedDek, org_id: &str) -> Result<Zeroizing<[u8; 32]>, VaultError> {
        let kek = self.keks.get(&w.kek_version).ok_or(VaultError::UnknownKekVersion(w.kek_version))?;
        if w.wrapped.len() <= NONCE_LEN {
            return Err(VaultError::Decrypt);
        }
        let (nonce, ct) = w.wrapped.split_at(NONCE_LEN);
        let raw = Self::open_raw(kek, ct, nonce, &aad(&["dek", org_id]))?;
        if raw.len() != 32 {
            return Err(VaultError::Decrypt);
        }
        let mut k = Zeroizing::new([0u8; 32]);
        k.copy_from_slice(&raw);
        Ok(k)
    }

    /// Re-wrap an org DEK under the current KEK (rotation). Idempotent.
    pub fn rewrap(&self, w: &WrappedDek, org_id: &str) -> Result<WrappedDek, VaultError> {
        if w.kek_version == self.current {
            return Ok(w.clone());
        }
        let dek = self.unwrap(w, org_id)?;
        self.wrap(&dek, org_id)
    }

    /// Seal a credential value for `(org_id, credential_id)`.
    pub fn seal(&self, w: &WrappedDek, org_id: &str, credential_id: &str, secret: &str) -> Result<Sealed, VaultError> {
        let dek = self.unwrap(w, org_id)?;
        let (ciphertext, nonce) = Self::seal_raw(&dek, secret.as_bytes(), &aad(&[org_id, credential_id]))?;
        Ok(Sealed { ciphertext, nonce })
    }

    /// Open a sealed credential just in time. The result zeroises on drop.
    pub fn open(&self, w: &WrappedDek, org_id: &str, credential_id: &str, sealed: &Sealed) -> Result<SecretString, VaultError> {
        let dek = self.unwrap(w, org_id)?;
        let raw = Self::open_raw(&dek, &sealed.ciphertext, &sealed.nonce, &aad(&[org_id, credential_id]))?;
        let s = String::from_utf8(raw.to_vec()).map_err(|_| VaultError::Decrypt)?;
        Ok(SecretString::from(s))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    fn vault() -> Vault {
        Vault::new(1, &Vault::generate_kek_b64()).unwrap()
    }

    #[test]
    fn round_trip() {
        let v = vault();
        let dek = v.new_org_dek("org_a").unwrap();
        let sealed = v.seal(&dek, "org_a", "cred_1", "brave-key-123").unwrap();
        assert_ne!(sealed.ciphertext, b"brave-key-123".to_vec());
        assert_eq!(sealed.nonce.len(), NONCE_LEN);
        assert_eq!(v.open(&dek, "org_a", "cred_1", &sealed).unwrap().expose_secret(), "brave-key-123");
    }

    #[test]
    fn nonces_are_unique() {
        let v = vault();
        let dek = v.new_org_dek("org_a").unwrap();
        let a = v.seal(&dek, "org_a", "c", "same").unwrap();
        let b = v.seal(&dek, "org_a", "c", "same").unwrap();
        assert_ne!(a.nonce, b.nonce);
        assert_ne!(a.ciphertext, b.ciphertext);
    }

    #[test]
    fn wrong_associated_data_is_rejected() {
        let v = vault();
        let dek = v.new_org_dek("org_a").unwrap();
        let sealed = v.seal(&dek, "org_a", "cred_1", "s3cret").unwrap();
        assert_eq!(v.open(&dek, "org_a", "cred_2", &sealed).unwrap_err(), VaultError::Decrypt, "moved to another row");
        assert_eq!(v.open(&dek, "org_b", "cred_1", &sealed).unwrap_err(), VaultError::Decrypt, "moved to another org");
        let mut tampered = sealed.clone();
        tampered.ciphertext[0] ^= 1;
        assert_eq!(v.open(&dek, "org_a", "cred_1", &tampered).unwrap_err(), VaultError::Decrypt);
    }

    #[test]
    fn another_orgs_dek_cannot_open() {
        let v = vault();
        let dek_a = v.new_org_dek("org_a").unwrap();
        let dek_b = v.new_org_dek("org_b").unwrap();
        let sealed = v.seal(&dek_a, "org_a", "c", "x").unwrap();
        assert!(v.open(&dek_b, "org_a", "c", &sealed).is_err(), "org_b's DEK is bound to org_b");
    }

    #[test]
    fn kek_rotation_rewraps_without_touching_credentials() {
        let old_kek = Vault::generate_kek_b64();
        let v1 = Vault::new(1, &old_kek).unwrap();
        let dek = v1.new_org_dek("org_a").unwrap();
        let sealed = v1.seal(&dek, "org_a", "c", "value").unwrap();

        let new_kek = Vault::generate_kek_b64();
        let v2 = Vault::new(2, &new_kek).unwrap().with_previous(1, &old_kek).unwrap();
        // Old rows still open under the previous KEK.
        assert_eq!(v2.open(&dek, "org_a", "c", &sealed).unwrap().expose_secret(), "value");
        let rewrapped = v2.rewrap(&dek, "org_a").unwrap();
        assert_eq!(rewrapped.kek_version, 2);
        assert_eq!(v2.rewrap(&rewrapped, "org_a").unwrap(), rewrapped, "idempotent");
        // After re-wrap the old KEK can be retired entirely.
        let v3 = Vault::new(2, &new_kek).unwrap();
        assert_eq!(v3.open(&rewrapped, "org_a", "c", &sealed).unwrap().expose_secret(), "value");
        assert_eq!(v3.open(&dek, "org_a", "c", &sealed).unwrap_err(), VaultError::UnknownKekVersion(1));
    }

    #[test]
    fn env_parsing_and_bad_keys() {
        let k1 = Vault::generate_kek_b64();
        let k2 = Vault::generate_kek_b64();
        let v = Vault::from_env_values(&k2, Some("2"), Some(&format!("1:{k1}"))).unwrap();
        assert_eq!(v.current_version(), 2);
        assert!(Vault::new(1, "c2hvcnQ=").is_err());
        assert!(Vault::new(1, "not base64!!").is_err());
        assert!(format!("{v:?}").contains("current"), "debug never prints keys");
    }
}
