//! Sealed storage for provider credentials saved from the dashboard.
//!
//! Secrets (Stripe secret key, Adyen API key, GoCardless access token, ...) are
//! sealed with AES-256-GCM before they touch the database. The key is derived
//! (SHA-256) from `RAGO_ENCRYPTION_KEY`; without it, saving a secret fails with
//! [`SecretError::KeyMissing`] instead of silently storing plaintext.
//!
//! Format: `v1:` + base64(nonce[12] || ciphertext || tag[16]).

use base64::Engine;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use sha2::{Digest, Sha256};

const PREFIX: &str = "v1:";
/// Env var holding the sealing key material (any high-entropy string).
pub const ENCRYPTION_KEY_ENV: &str = "RAGO_ENCRYPTION_KEY";

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("RAGO_ENCRYPTION_KEY is not set; provider secrets cannot be stored")]
    KeyMissing,
    #[error("secret could not be sealed")]
    Seal,
    #[error("secret could not be opened (wrong RAGO_ENCRYPTION_KEY or corrupted value)")]
    Open,
}

fn key_from_env() -> Result<LessSafeKey, SecretError> {
    let material = std::env::var(ENCRYPTION_KEY_ENV)
        .ok()
        .filter(|v| !v.trim().is_empty())
        .ok_or(SecretError::KeyMissing)?;
    key_from_material(&material)
}

fn key_from_material(material: &str) -> Result<LessSafeKey, SecretError> {
    let digest = Sha256::digest(material.as_bytes());
    let unbound = UnboundKey::new(&AES_256_GCM, &digest).map_err(|_| SecretError::Seal)?;
    Ok(LessSafeKey::new(unbound))
}

/// Whether a sealing key is configured.
pub fn is_configured() -> bool {
    key_from_env().is_ok()
}

/// Seal `plaintext` with the configured key.
pub fn seal(plaintext: &str) -> Result<String, SecretError> {
    seal_with(&key_from_env()?, plaintext)
}

/// Open a value produced by [`seal`].
pub fn open(sealed: &str) -> Result<String, SecretError> {
    open_with(&key_from_env()?, sealed)
}

fn seal_with(key: &LessSafeKey, plaintext: &str) -> Result<String, SecretError> {
    let mut nonce_bytes = [0u8; NONCE_LEN];
    SystemRandom::new()
        .fill(&mut nonce_bytes)
        .map_err(|_| SecretError::Seal)?;
    let mut in_out = plaintext.as_bytes().to_vec();
    key.seal_in_place_append_tag(
        Nonce::assume_unique_for_key(nonce_bytes),
        Aad::empty(),
        &mut in_out,
    )
    .map_err(|_| SecretError::Seal)?;
    let mut out = nonce_bytes.to_vec();
    out.extend_from_slice(&in_out);
    Ok(format!(
        "{PREFIX}{}",
        base64::engine::general_purpose::STANDARD.encode(out)
    ))
}

fn open_with(key: &LessSafeKey, sealed: &str) -> Result<String, SecretError> {
    let body = sealed.strip_prefix(PREFIX).ok_or(SecretError::Open)?;
    let raw = base64::engine::general_purpose::STANDARD
        .decode(body)
        .map_err(|_| SecretError::Open)?;
    if raw.len() <= NONCE_LEN {
        return Err(SecretError::Open);
    }
    let (nonce, cipher) = raw.split_at(NONCE_LEN);
    let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| SecretError::Open)?;
    let mut in_out = cipher.to_vec();
    let plain = key
        .open_in_place(nonce, Aad::empty(), &mut in_out)
        .map_err(|_| SecretError::Open)?;
    String::from_utf8(plain.to_vec()).map_err(|_| SecretError::Open)
}

/// `****last4` display mask for a secret.
pub fn mask(secret: &str) -> String {
    if secret.len() <= 4 {
        "****".to_string()
    } else {
        format!("****{}", &secret[secret.len() - 4..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_rejects_wrong_key() {
        let k = key_from_material("test-material").unwrap();
        let sealed = seal_with(&k, "sk_live_123").unwrap();
        assert!(sealed.starts_with(PREFIX));
        assert_eq!(open_with(&k, &sealed).unwrap(), "sk_live_123");
        let other = key_from_material("other").unwrap();
        assert!(open_with(&other, &sealed).is_err());
        assert_eq!(mask("sk_live_1234"), "****1234");
    }
}
