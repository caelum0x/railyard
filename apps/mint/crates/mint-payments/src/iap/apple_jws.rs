//! Apple-signed JWS (App Store Server Notifications v2, signed transactions
//! and renewal info): ES256 over `header.payload`, with the signing key in an
//! `x5c` chain of leaf, intermediate and root that must end at a PINNED root
//! (Apple Root CA - G3 by SHA-256 fingerprint), each link signed by the next,
//! all valid now, and carrying Apple's marker extensions.

use base64::Engine;
use ring::signature::{UnparsedPublicKey, ECDSA_P256_SHA256_FIXED};
use serde_json::Value;
use sha2::{Digest, Sha256};
use x509_parser::prelude::{FromDer, X509Certificate};
use x509_parser::time::ASN1Time;

/// SHA-256 of Apple Root CA - G3 (DER), from Apple PKI.
pub const APPLE_ROOT_CA_G3_SHA256: &str = "63343abfb89a6a03ebb57e9b3f5fa7be7c4f5c756f3017b3a8c488c3653e9179";
/// Marker extension on Apple's App Store signing leaf certificates.
pub const LEAF_OID: &str = "1.2.840.113635.100.6.11.1";
/// Marker extension on Apple's WWDR intermediate.
pub const INTERMEDIATE_OID: &str = "1.2.840.113635.100.6.2.1";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JwsError {
    #[error("malformed JWS: {0}")]
    Malformed(&'static str),
    #[error("unsupported algorithm")]
    Algorithm,
    #[error("certificate chain does not end at the pinned root")]
    UntrustedRoot,
    #[error("certificate chain signature is invalid")]
    BrokenChain,
    #[error("a certificate is outside its validity period")]
    Expired,
    #[error("a certificate lacks Apple's marker extension")]
    NotApple,
    #[error("JWS signature is invalid")]
    BadSignature,
}

/// Verifies Apple JWS against a pinned root.
#[derive(Debug, Clone)]
pub struct AppleJwsVerifier {
    root_sha256: String,
}

impl Default for AppleJwsVerifier {
    fn default() -> Self {
        Self::new(APPLE_ROOT_CA_G3_SHA256)
    }
}

const B64URL: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

fn parse_cert(der: &[u8]) -> Result<X509Certificate<'_>, JwsError> {
    X509Certificate::from_der(der).map(|(_, c)| c).map_err(|_| JwsError::Malformed("certificate DER"))
}

fn has_ext(cert: &X509Certificate<'_>, oid: &str) -> bool {
    cert.extensions().iter().any(|e| e.oid.to_id_string() == oid)
}

impl AppleJwsVerifier {
    /// Pin a root by the lowercase hex SHA-256 of its DER.
    pub fn new(root_sha256_hex: &str) -> Self {
        Self { root_sha256: root_sha256_hex.trim().to_ascii_lowercase().replace(':', "") }
    }

    /// Verify `jws` at unix time `now` and return its payload.
    pub fn verify(&self, jws: &str, now: i64) -> Result<Value, JwsError> {
        let mut parts = jws.split('.');
        let (Some(h), Some(p), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
            return Err(JwsError::Malformed("expected three segments"));
        };
        let header: Value = serde_json::from_slice(&B64URL.decode(h).map_err(|_| JwsError::Malformed("header encoding"))?).map_err(|_| JwsError::Malformed("header JSON"))?;
        if header["alg"].as_str() != Some("ES256") {
            return Err(JwsError::Algorithm);
        }
        let x5c: Vec<Vec<u8>> = header["x5c"]
            .as_array()
            .ok_or(JwsError::Malformed("no x5c"))?
            .iter()
            .map(|c| c.as_str().and_then(|c| B64.decode(c).ok()).ok_or(JwsError::Malformed("x5c entry")))
            .collect::<Result<_, _>>()?;
        if x5c.len() != 3 {
            return Err(JwsError::Malformed("x5c must hold leaf, intermediate and root"));
        }
        let root_fp: String = Sha256::digest(&x5c[2]).iter().map(|b| format!("{b:02x}")).collect();
        if root_fp != self.root_sha256 {
            return Err(JwsError::UntrustedRoot);
        }
        let (leaf, inter, root) = (parse_cert(&x5c[0])?, parse_cert(&x5c[1])?, parse_cert(&x5c[2])?);
        let at = ASN1Time::from_timestamp(now).map_err(|_| JwsError::Expired)?;
        if ![&leaf, &inter, &root].iter().all(|c| c.validity().is_valid_at(at)) {
            return Err(JwsError::Expired);
        }
        inter.verify_signature(Some(root.public_key())).map_err(|_| JwsError::BrokenChain)?;
        leaf.verify_signature(Some(inter.public_key())).map_err(|_| JwsError::BrokenChain)?;
        if !has_ext(&leaf, LEAF_OID) || !has_ext(&inter, INTERMEDIATE_OID) {
            return Err(JwsError::NotApple);
        }
        let sig = B64URL.decode(s).map_err(|_| JwsError::Malformed("signature encoding"))?;
        let key = UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, leaf.public_key().subject_public_key.data.as_ref());
        key.verify(format!("{h}.{p}").as_bytes(), &sig).map_err(|_| JwsError::BadSignature)?;
        serde_json::from_slice(&B64URL.decode(p).map_err(|_| JwsError::Malformed("payload encoding"))?).map_err(|_| JwsError::Malformed("payload JSON"))
    }
}

/// Test support: an Apple-shaped chain and a signer.
#[cfg(test)]
pub mod testing {
    use super::*;
    use rcgen::{BasicConstraints, CertificateParams, CertifiedIssuer, CustomExtension, IsCa, KeyPair, PKCS_ECDSA_P256_SHA256};
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};

    /// A root, an intermediate and a leaf shaped like Apple's.
    pub struct Chain {
        pub x5c: Vec<String>,
        pub root_sha256: String,
        leaf_pkcs8: Vec<u8>,
    }

    fn oid(s: &str) -> Vec<u64> {
        s.split('.').map(|p| p.parse().unwrap_or(0)).collect()
    }

    impl Chain {
        /// `apple_marks`: put Apple's marker extensions on leaf and intermediate.
        pub fn new(apple_marks: bool) -> Self {
            let ca = |name: &str| {
                let mut p = CertificateParams::new(Vec::<String>::new()).expect("params");
                p.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
                p.distinguished_name.push(rcgen::DnType::CommonName, name);
                p
            };
            let root_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("root key");
            let root = CertifiedIssuer::self_signed(ca("Test Root CA - G3"), root_key).expect("root");
            let mut ip = ca("Test WWDR");
            if apple_marks {
                ip.custom_extensions.push(CustomExtension::from_oid_content(&oid(INTERMEDIATE_OID), vec![0x05, 0x00]));
            }
            let inter_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("intermediate key");
            let inter = CertifiedIssuer::signed_by(ip, inter_key, &root).expect("intermediate");
            let mut lp = CertificateParams::new(Vec::<String>::new()).expect("params");
            lp.distinguished_name.push(rcgen::DnType::CommonName, "Test Prod ECC Mac App Store");
            if apple_marks {
                lp.custom_extensions.push(CustomExtension::from_oid_content(&oid(LEAF_OID), vec![0x05, 0x00]));
            }
            let leaf_key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).expect("leaf key");
            let leaf = lp.signed_by(&leaf_key, &inter).expect("leaf");
            let der = |d: &[u8]| B64.encode(d);
            Chain {
                x5c: vec![der(leaf.der()), der(inter.der()), der(root.der())],
                root_sha256: Sha256::digest(root.der()).iter().map(|b| format!("{b:02x}")).collect(),
                leaf_pkcs8: leaf_key.serialize_der(),
            }
        }

        /// Sign `payload` as an Apple JWS with this chain.
        pub fn sign(&self, payload: &Value) -> String {
            let header = serde_json::json!({ "alg": "ES256", "x5c": self.x5c });
            let input = format!("{}.{}", B64URL.encode(header.to_string()), B64URL.encode(payload.to_string()));
            let rng = SystemRandom::new();
            let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &self.leaf_pkcs8, &rng).expect("leaf pkcs8");
            let sig = key.sign(&rng, input.as_bytes()).expect("sign");
            format!("{input}.{}", B64URL.encode(sig.as_ref()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::Chain;
    use super::*;
    use serde_json::json;

    fn now() -> i64 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
    }

    #[test]
    fn a_valid_chain_and_signature_verify() {
        let chain = Chain::new(true);
        let jws = chain.sign(&json!({ "notificationType": "ONE_TIME_CHARGE" }));
        let v = AppleJwsVerifier::new(&chain.root_sha256).verify(&jws, now()).unwrap();
        assert_eq!(v["notificationType"], "ONE_TIME_CHARGE");
    }

    #[test]
    fn untrusted_roots_tampering_and_unmarked_chains_fail() {
        let chain = Chain::new(true);
        let jws = chain.sign(&json!({ "a": 1 }));
        assert_eq!(AppleJwsVerifier::default().verify(&jws, now()), Err(JwsError::UntrustedRoot));
        let v = AppleJwsVerifier::new(&chain.root_sha256);
        let mut parts: Vec<&str> = jws.split('.').collect();
        let forged = B64URL.encode(json!({ "a": 2 }).to_string());
        parts[1] = &forged;
        assert_eq!(v.verify(&parts.join("."), now()), Err(JwsError::BadSignature));
        assert_eq!(v.verify(&jws, 86_400), Err(JwsError::Expired), "before not_before");
        let unmarked = Chain::new(false);
        assert_eq!(AppleJwsVerifier::new(&unmarked.root_sha256).verify(&unmarked.sign(&json!({})), now()), Err(JwsError::NotApple));
        assert!(matches!(v.verify("a.b", now()), Err(JwsError::Malformed(_))));
    }
}
