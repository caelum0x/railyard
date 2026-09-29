//! Seller credentials at rest (Wave P PY2.3): AES-256-GCM with a 32-byte key
//! from `MINT_SELLER_SEAL_KEY` (base64), a fresh 96-bit nonce per seal, and
//! the seller key as associated data so a sealed value cannot be moved to
//! another seller's row.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};

/// A sealing key. `Debug` shows nothing.
pub struct SealKey(LessSafeKey);

impl std::fmt::Debug for SealKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SealKey([hidden])")
    }
}

impl SealKey {
    /// From `MINT_SELLER_SEAL_KEY` (standard base64 of exactly 32 bytes).
    ///
    /// # Errors
    /// When the value is not base64 of 32 bytes.
    pub fn from_base64(raw: &str) -> Result<Self, String> {
        let bytes = STANDARD.decode(raw.trim()).map_err(|_| "MINT_SELLER_SEAL_KEY is not base64".to_owned())?;
        if bytes.len() != 32 {
            return Err("MINT_SELLER_SEAL_KEY must be 32 bytes".to_owned());
        }
        let key = UnboundKey::new(&AES_256_GCM, &bytes).map_err(|_| "MINT_SELLER_SEAL_KEY is not usable".to_owned())?;
        Ok(Self(LessSafeKey::new(key)))
    }

    /// `base64(nonce || ciphertext || tag)` bound to `context`.
    ///
    /// # Errors
    /// When the system random source fails.
    pub fn seal(&self, context: &str, plain: &str) -> Result<String, String> {
        let mut nonce = [0u8; NONCE_LEN];
        SystemRandom::new().fill(&mut nonce).map_err(|_| "no random source".to_owned())?;
        let mut data = plain.as_bytes().to_vec();
        self.0
            .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from(context.as_bytes()), &mut data)
            .map_err(|_| "could not seal".to_owned())?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&data);
        Ok(STANDARD.encode(out))
    }

    /// Opens a value sealed for `context`.
    ///
    /// # Errors
    /// When the value was sealed with another key or for another context.
    pub fn open(&self, context: &str, sealed: &str) -> Result<String, String> {
        let raw = STANDARD.decode(sealed).map_err(|_| "sealed value is not base64".to_owned())?;
        if raw.len() < NONCE_LEN + 16 {
            return Err("sealed value is too short".to_owned());
        }
        let (nonce, body) = raw.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| "bad nonce".to_owned())?;
        let mut body = body.to_vec();
        let plain = self
            .0
            .open_in_place(nonce, Aad::from(context.as_bytes()), &mut body)
            .map_err(|_| "sealed value does not open with this key and seller".to_owned())?;
        String::from_utf8(plain.to_vec()).map_err(|_| "sealed value is not text".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::SealKey;

    const KEY: &str = "MDEyMzQ1Njc4OWFiY2RlZjAxMjM0NTY3ODlhYmNkZWY=";

    #[test]
    fn seals_open_only_with_the_same_key_and_seller() {
        let key = SealKey::from_base64(KEY).expect("key");
        let sealed = key.seal("scribase:org.shop.production", "dodo_secret_key").expect("seal");
        assert!(!sealed.contains("dodo_secret_key"));
        assert_eq!(key.open("scribase:org.shop.production", &sealed).expect("open"), "dodo_secret_key");
        assert!(key.open("scribase:other.seller", &sealed).is_err(), "bound to its seller");
        let other = SealKey::from_base64("ZmVkY2JhOTg3NjU0MzIxMGZlZGNiYTk4NzY1NDMyMTA=").expect("key");
        assert!(other.open("scribase:org.shop.production", &sealed).is_err());
        assert_ne!(key.seal("c", "x").expect("a"), key.seal("c", "x").expect("b"), "fresh nonce each time");
        assert!(SealKey::from_base64("c2hvcnQ=").is_err());
        assert_eq!(format!("{key:?}"), "SealKey([hidden])");
    }
}
