//! RS256 webhook signatures (Lago `signature_algo: jwt`).
//!
//! Lago signs `X-Lago-Signature` as a JWT over `{ "data": <payload JSON
//! string>, "iss": <API URL> }` with the deployment's RSA key, and publishes
//! the public key at `GET /webhooks/public_key` (base64 of the PEM) so a
//! receiver verifies without a shared secret.
//!
//! The key comes from `RAGO_RSA_PRIVATE_KEY` (PKCS#8 or PKCS#1 PEM, raw or
//! base64-encoded, like Lago's `LAGO_RSA_PRIVATE_KEY`); the issuer from
//! `RAGO_API_URL` (else `LAGO_API_URL`).

use base64::Engine;
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs1v15::SigningKey;
use rsa::pkcs8::{DecodePrivateKey, EncodePublicKey, LineEnding};
use rsa::signature::{SignatureEncoding, Signer};
use rsa::RsaPrivateKey;
use sha2::Sha256;

const B64URL: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// A loaded RS256 webhook signer.
#[derive(Clone)]
pub struct JwtSigner {
    key: SigningKey<Sha256>,
    public_pem: String,
    issuer: String,
}

impl std::fmt::Debug for JwtSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JwtSigner").field("issuer", &self.issuer).finish_non_exhaustive()
    }
}

/// Why a key could not be loaded.
#[derive(Debug, thiserror::Error)]
pub enum JwtKeyError {
    #[error("RAGO_RSA_PRIVATE_KEY is not an RSA private key in PEM form")]
    Invalid,
}

impl JwtSigner {
    /// From a PEM (or base64 of a PEM) private key.
    pub fn from_pem(raw: &str, issuer: impl Into<String>) -> Result<Self, JwtKeyError> {
        let text = raw.trim();
        let pem = if text.starts_with("-----BEGIN") {
            text.to_string()
        } else {
            let bytes = base64::engine::general_purpose::STANDARD.decode(text.replace(['\n', '\r'], "")).map_err(|_| JwtKeyError::Invalid)?;
            String::from_utf8(bytes).map_err(|_| JwtKeyError::Invalid)?
        };
        let key = RsaPrivateKey::from_pkcs8_pem(&pem).or_else(|_| RsaPrivateKey::from_pkcs1_pem(&pem)).map_err(|_| JwtKeyError::Invalid)?;
        let public_pem = key.to_public_key().to_public_key_pem(LineEnding::LF).map_err(|_| JwtKeyError::Invalid)?;
        Ok(Self { key: SigningKey::<Sha256>::new(key), public_pem, issuer: issuer.into() })
    }

    /// From the environment; `None` when no key is configured.
    pub fn from_env() -> Result<Option<Self>, JwtKeyError> {
        let Some(raw) = std::env::var("RAGO_RSA_PRIVATE_KEY").ok().filter(|v| !v.trim().is_empty()) else {
            return Ok(None);
        };
        let issuer = std::env::var("RAGO_API_URL").or_else(|_| std::env::var("LAGO_API_URL")).unwrap_or_default();
        Self::from_pem(&raw, issuer).map(Some)
    }

    /// The process-wide signer from the environment, loaded once. A malformed
    /// key is treated as absent (endpoints then sign with HMAC) and logged.
    pub fn global() -> Option<&'static JwtSigner> {
        static SIGNER: std::sync::OnceLock<Option<JwtSigner>> = std::sync::OnceLock::new();
        SIGNER
            .get_or_init(|| match Self::from_env() {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("rago: {e}; webhook JWT signing disabled");
                    None
                }
            })
            .as_ref()
    }

    /// The SPKI public key PEM.
    pub fn public_key_pem(&self) -> &str {
        &self.public_pem
    }

    /// `GET /webhooks/public_key`: base64 of the PEM (Lago's shape).
    pub fn public_key_base64(&self) -> String {
        base64::engine::general_purpose::STANDARD.encode(self.public_pem.as_bytes())
    }

    /// The `X-Lago-Signature` JWT for a payload JSON string.
    pub fn sign(&self, payload_json: &str) -> String {
        let header = B64URL.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let claims = B64URL.encode(serde_json::json!({ "data": payload_json, "iss": self.issuer }).to_string());
        let input = format!("{header}.{claims}");
        let sig = self.key.sign(input.as_bytes());
        format!("{input}.{}", B64URL.encode(sig.to_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::pkcs1v15::{Signature, VerifyingKey};
    use rsa::pkcs8::{DecodePublicKey, EncodePrivateKey};
    use rsa::signature::Verifier;
    use rsa::RsaPublicKey;

    fn signer() -> JwtSigner {
        let key = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
        let pem = key.to_pkcs8_pem(LineEnding::LF).unwrap();
        JwtSigner::from_pem(&base64::engine::general_purpose::STANDARD.encode(pem.as_bytes()), "https://billing.example").unwrap()
    }

    #[test]
    fn the_signature_verifies_with_the_published_key_and_carries_the_payload() {
        let s = signer();
        let token = s.sign(r#"{"webhook_type":"invoice.created"}"#);
        let parts: Vec<&str> = token.split('.').collect();
        assert_eq!(parts.len(), 3);
        let published = String::from_utf8(base64::engine::general_purpose::STANDARD.decode(s.public_key_base64()).unwrap()).unwrap();
        let public = RsaPublicKey::from_public_key_pem(&published).unwrap();
        let verifier = VerifyingKey::<Sha256>::new(public);
        let sig = Signature::try_from(B64URL.decode(parts[2]).unwrap().as_slice()).unwrap();
        verifier.verify(format!("{}.{}", parts[0], parts[1]).as_bytes(), &sig).expect("RS256 verifies");
        let claims: serde_json::Value = serde_json::from_slice(&B64URL.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims["data"], r#"{"webhook_type":"invoice.created"}"#);
        assert_eq!(claims["iss"], "https://billing.example");
        // A changed payload does not verify.
        assert!(verifier.verify(b"x.y", &sig).is_err());
    }

    #[test]
    fn junk_is_not_a_key() {
        assert!(JwtSigner::from_pem("not a key", "").is_err());
        assert!(!format!("{:?}", signer()).contains("PRIVATE"));
    }
}

#[cfg(test)]
mod header_tests {
    use super::*;
    use crate::webhooks::{Signing, Webhook};
    use rsa::pkcs8::EncodePrivateKey;

    #[test]
    fn jwt_endpoints_get_jwt_headers() {
        let key = RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
        let s = JwtSigner::from_pem(&key.to_pkcs8_pem(LineEnding::LF).unwrap(), "i").unwrap();
        let wh = Webhook::build(uuid::Uuid::new_v4(), "https://x.example/h", "invoice.created", "invoice", serde_json::json!({"a": 1}));
        let h = wh.signed_headers(Signing::Jwt(&s));
        assert!(h.contains(&("X-Lago-Signature-Algorithm".into(), "jwt".into())));
        assert_eq!(h.iter().find(|(k, _)| k == "X-Lago-Signature").unwrap().1.split('.').count(), 3);
        let h = wh.signed_headers(Signing::Hmac("k"));
        assert!(h.contains(&("X-Lago-Signature-Algorithm".into(), "hmac".into())));
    }
}
