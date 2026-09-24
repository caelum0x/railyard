//! Coinbase Developer Platform (CDP) facilitator authentication.
//!
//! The CDP-hosted x402 facilitator (`https://api.cdp.coinbase.com/platform/v2/x402`)
//! rejects anonymous calls. Every request carries `Authorization: Bearer <jwt>`
//! where the JWT is minted per request from a CDP Secret API key, exactly as the
//! official CDP SDKs do (`generateJwt` in `@coinbase/cdp-sdk`, `auth.GenerateJWT`
//! in the Go SDK):
//!
//! - header: `{ "alg": "ES256" | "EdDSA", "kid": <key id>, "typ": "JWT", "nonce": <16 random bytes, hex> }`
//! - claims: `{ "sub": <key id>, "iss": "cdp", "nbf": now, "iat": now, "exp": now + 120,
//!   "uris": ["<METHOD> <host><path>"] }`
//!
//! Two key formats are issued by the CDP portal and both are accepted:
//!
//! - **ECDSA** (legacy): a PEM `EC PRIVATE KEY` (SEC1) or `PRIVATE KEY` (PKCS#8)
//!   on P-256, signed with ES256;
//! - **Ed25519** (current default): base64 of 64 bytes (32-byte seed followed by
//!   the 32-byte public key), signed with EdDSA.
//!
//! A JWT is bound to one method + host + path and lives 120 seconds, so it is
//! generated fresh for each `/verify` and `/settle` call.

use std::fmt;

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use ed25519_dalek::Signer as _;
use p256::pkcs8::DecodePrivateKey;
use rand::RngCore;
use serde_json::json;

use crate::clock::now_ms;

/// The CDP-hosted x402 facilitator base URL (mainnet and Base Sepolia).
pub const CDP_FACILITATOR_URL: &str = "https://api.cdp.coinbase.com/platform/v2/x402";
/// Host of the CDP API; a facilitator URL on this host needs a CDP key.
pub const CDP_API_HOST: &str = "api.cdp.coinbase.com";
/// JWT lifetime used by the CDP SDKs.
pub const CDP_JWT_TTL_SECS: i64 = 120;
/// `Correlation-Context` sent with every CDP call (the SDKs send the same shape).
const CORRELATION_CONTEXT: &str = concat!(
    "sdkLanguage=rust,source=turnstile,sourceVersion=",
    env!("CARGO_PKG_VERSION")
);

enum SigningKey {
    Es256(p256::ecdsa::SigningKey),
    Ed25519(ed25519_dalek::SigningKey),
}

impl Clone for SigningKey {
    fn clone(&self) -> Self {
        match self {
            Self::Es256(key) => Self::Es256(key.clone()),
            Self::Ed25519(key) => Self::Ed25519(key.clone()),
        }
    }
}

/// A parsed CDP Secret API key. `Debug` never prints the secret.
#[derive(Clone)]
pub struct CdpApiKey {
    key_id: String,
    key: SigningKey,
}

impl fmt::Debug for CdpApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CdpApiKey")
            .field("key_id", &self.key_id)
            .field("algorithm", &self.algorithm())
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl PartialEq for CdpApiKey {
    fn eq(&self, other: &Self) -> bool {
        self.key_id == other.key_id && self.public_key_bytes() == other.public_key_bytes()
    }
}

impl Eq for CdpApiKey {}

impl CdpApiKey {
    /// Parse a CDP key id + secret as downloaded from the CDP portal. Env
    /// files often carry a PEM with literal `\n` escapes; those are unescaped.
    pub fn parse(key_id: &str, secret: &str) -> Result<Self, String> {
        let key_id = key_id.trim();
        if key_id.is_empty() {
            return Err("the CDP API key id is empty".into());
        }
        let secret = secret.trim().replace("\\n", "\n");
        if secret.is_empty() {
            return Err("the CDP API key secret is empty".into());
        }
        let key = if secret.starts_with("-----BEGIN") {
            parse_pem(&secret)?
        } else {
            parse_ed25519(&secret)?
        };
        Ok(Self {
            key_id: key_id.to_string(),
            key,
        })
    }

    #[must_use]
    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// The JWS `alg` this key signs with.
    #[must_use]
    pub fn algorithm(&self) -> &'static str {
        match self.key {
            SigningKey::Es256(_) => "ES256",
            SigningKey::Ed25519(_) => "EdDSA",
        }
    }

    /// The public key (SEC1 uncompressed for ES256, raw 32 bytes for EdDSA).
    #[must_use]
    pub fn public_key_bytes(&self) -> Vec<u8> {
        match &self.key {
            SigningKey::Es256(key) => key
                .verifying_key()
                .to_encoded_point(false)
                .as_bytes()
                .to_vec(),
            SigningKey::Ed25519(key) => key.verifying_key().to_bytes().to_vec(),
        }
    }

    /// A JWT for one request, with an explicit clock and nonce (deterministic
    /// inputs for tests; production uses [`bearer`](Self::bearer)).
    #[must_use]
    pub fn jwt_at(
        &self,
        method: &str,
        host: &str,
        path: &str,
        now_secs: i64,
        nonce: &[u8; 16],
    ) -> String {
        let header = json!({
            "alg": self.algorithm(),
            "kid": self.key_id,
            "typ": "JWT",
            "nonce": hex::encode(nonce),
        });
        let claims = json!({
            "sub": self.key_id,
            "iss": "cdp",
            "nbf": now_secs,
            "iat": now_secs,
            "exp": now_secs + CDP_JWT_TTL_SECS,
            "uris": [format!("{} {}{}", method.to_ascii_uppercase(), host, path)],
        });
        let signing_input = format!(
            "{}.{}",
            URL_SAFE_NO_PAD.encode(header.to_string()),
            URL_SAFE_NO_PAD.encode(claims.to_string())
        );
        let signature = match &self.key {
            SigningKey::Es256(key) => {
                let signature: p256::ecdsa::Signature = key.sign(signing_input.as_bytes());
                signature.to_bytes().to_vec()
            }
            SigningKey::Ed25519(key) => key.sign(signing_input.as_bytes()).to_bytes().to_vec(),
        };
        format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature))
    }

    /// `Bearer <jwt>` for one request, minted now with a fresh random nonce.
    #[must_use]
    pub fn bearer(&self, method: &str, host: &str, path: &str) -> String {
        let mut nonce = [0_u8; 16];
        rand::thread_rng().fill_bytes(&mut nonce);
        let now_secs = now_ms() / 1000;
        format!(
            "Bearer {}",
            self.jwt_at(method, host, path, now_secs, &nonce)
        )
    }
}

fn parse_pem(pem: &str) -> Result<SigningKey, String> {
    if pem.contains("BEGIN EC PRIVATE KEY") {
        let secret = p256::SecretKey::from_sec1_pem(pem).map_err(|_| {
            "the CDP API key secret is a PEM EC key but not a valid P-256 (prime256v1) key"
                .to_string()
        })?;
        return Ok(SigningKey::Es256(secret.into()));
    }
    if pem.contains("BEGIN PRIVATE KEY") {
        return p256::ecdsa::SigningKey::from_pkcs8_pem(pem)
            .map(SigningKey::Es256)
            .map_err(|_| {
                "the CDP API key secret is a PKCS#8 PEM but not a P-256 EC key".to_string()
            });
    }
    Err("the CDP API key secret PEM must be an `EC PRIVATE KEY` or `PRIVATE KEY` block".into())
}

fn parse_ed25519(secret: &str) -> Result<SigningKey, String> {
    let bytes = STANDARD.decode(secret).map_err(|_| {
        "the CDP API key secret must be a PEM EC key or a base64 Ed25519 key".to_string()
    })?;
    let pair: [u8; 64] = bytes.as_slice().try_into().map_err(|_| {
        format!(
            "the CDP Ed25519 key secret must decode to 64 bytes (seed + public key), got {}",
            bytes.len()
        )
    })?;
    ed25519_dalek::SigningKey::from_keypair_bytes(&pair)
        .map(SigningKey::Ed25519)
        .map_err(|_| "the CDP Ed25519 key secret's public half does not match its seed".to_string())
}

/// Per-request CDP auth bound to one facilitator base URL: the JWT `uris`
/// claim names the facilitator host and the full operation path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CdpFacilitatorAuth {
    key: CdpApiKey,
    host: String,
    base_path: String,
}

impl CdpFacilitatorAuth {
    /// `facilitator_url` is the base URL `/verify` and `/settle` hang off.
    pub fn new(key: CdpApiKey, facilitator_url: &str) -> Result<Self, String> {
        let (host, base_path) = split_url(facilitator_url)?;
        Ok(Self {
            key,
            host,
            base_path,
        })
    }

    #[must_use]
    pub fn key(&self) -> &CdpApiKey {
        &self.key
    }

    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The full request path for an operation (`/verify` → `/platform/v2/x402/verify`).
    #[must_use]
    pub fn full_path(&self, operation: &str) -> String {
        format!("{}{}", self.base_path, operation)
    }

    /// Headers for one facilitator call.
    #[must_use]
    pub fn headers(&self, method: &str, operation: &str) -> Vec<(String, String)> {
        vec![
            (
                "authorization".to_string(),
                self.key
                    .bearer(method, &self.host, &self.full_path(operation)),
            ),
            (
                "correlation-context".to_string(),
                CORRELATION_CONTEXT.to_string(),
            ),
        ]
    }
}

/// Split `https://host[:port]/base/path/` into (`host[:port]`, `/base/path`).
pub fn split_url(url: &str) -> Result<(String, String), String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .ok_or_else(|| format!("facilitator URL {url:?} is not an http(s) URL"))?;
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let (host, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    if host.is_empty() {
        return Err(format!("facilitator URL {url:?} has no host"));
    }
    Ok((
        host.to_ascii_lowercase(),
        path.trim_end_matches('/').to_string(),
    ))
}

/// `true` when `url` points at the CDP API host.
#[must_use]
pub fn is_cdp_url(url: &str) -> bool {
    split_url(url).is_ok_and(|(host, _)| host == CDP_API_HOST)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ed25519_dalek::Verifier as _;
    use p256::pkcs8::{EncodePrivateKey, LineEnding};
    use serde_json::Value;

    const KEY_ID: &str = "organizations/org-1/apiKeys/key-1";
    const NONCE: [u8; 16] = [7; 16];

    pub(crate) fn ec_pem() -> String {
        let secret = p256::SecretKey::from_slice(&[0x11; 32]).unwrap();
        secret.to_sec1_pem(LineEnding::LF).unwrap().to_string()
    }

    pub(crate) fn ed25519_secret() -> String {
        let key = ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]);
        STANDARD.encode(key.to_keypair_bytes())
    }

    fn decode_part(part: &str) -> Value {
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(part).unwrap()).unwrap()
    }

    fn check_claims(jwt: &str, alg: &str) -> (String, Vec<u8>) {
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3);
        let header = decode_part(parts[0]);
        assert_eq!(header["alg"], alg);
        assert_eq!(header["kid"], KEY_ID);
        assert_eq!(header["typ"], "JWT");
        assert_eq!(header["nonce"], hex::encode(NONCE));
        let claims = decode_part(parts[1]);
        assert_eq!(claims["sub"], KEY_ID);
        assert_eq!(claims["iss"], "cdp");
        assert_eq!(claims["nbf"], 1_700_000_000);
        assert_eq!(claims["exp"], 1_700_000_000 + CDP_JWT_TTL_SECS);
        assert_eq!(
            claims["uris"][0],
            "POST api.cdp.coinbase.com/platform/v2/x402/verify"
        );
        (
            format!("{}.{}", parts[0], parts[1]),
            URL_SAFE_NO_PAD.decode(parts[2]).unwrap(),
        )
    }

    #[test]
    fn es256_jwt_matches_the_cdp_shape_and_verifies() {
        let key = CdpApiKey::parse(KEY_ID, &ec_pem()).unwrap();
        assert_eq!(key.algorithm(), "ES256");
        let jwt = key.jwt_at(
            "post",
            CDP_API_HOST,
            "/platform/v2/x402/verify",
            1_700_000_000,
            &NONCE,
        );
        let (input, sig) = check_claims(&jwt, "ES256");
        assert_eq!(sig.len(), 64, "JWS ES256 is raw r||s");
        let verifying =
            p256::ecdsa::VerifyingKey::from_sec1_bytes(&key.public_key_bytes()).unwrap();
        let signature = p256::ecdsa::Signature::from_slice(&sig).unwrap();
        verifying.verify(input.as_bytes(), &signature).unwrap();
    }

    #[test]
    fn ed25519_jwt_matches_the_cdp_shape_and_verifies() {
        let key = CdpApiKey::parse("5b5b6c1e-uuid-key", &ed25519_secret()).unwrap();
        assert_eq!(key.algorithm(), "EdDSA");
        let key = CdpApiKey {
            key_id: KEY_ID.into(),
            ..key
        };
        let jwt = key.jwt_at(
            "POST",
            CDP_API_HOST,
            "/platform/v2/x402/verify",
            1_700_000_000,
            &NONCE,
        );
        let (input, sig) = check_claims(&jwt, "EdDSA");
        let pk: [u8; 32] = key.public_key_bytes().try_into().unwrap();
        let verifying = ed25519_dalek::VerifyingKey::from_bytes(&pk).unwrap();
        let signature = ed25519_dalek::Signature::from_slice(&sig).unwrap();
        verifying.verify(input.as_bytes(), &signature).unwrap();
    }

    #[test]
    fn pkcs8_and_escaped_newline_pems_are_accepted() {
        let pkcs8 = p256::SecretKey::from_slice(&[0x33; 32])
            .unwrap()
            .to_pkcs8_pem(LineEnding::LF)
            .unwrap()
            .to_string();
        assert_eq!(
            CdpApiKey::parse(KEY_ID, &pkcs8).unwrap().algorithm(),
            "ES256"
        );
        let escaped = ec_pem().replace('\n', "\\n");
        assert_eq!(
            CdpApiKey::parse(KEY_ID, &escaped).unwrap(),
            CdpApiKey::parse(KEY_ID, &ec_pem()).unwrap()
        );
    }

    #[test]
    fn malformed_secrets_fail_with_a_clear_reason() {
        let short = CdpApiKey::parse(KEY_ID, &STANDARD.encode([1_u8; 32])).unwrap_err();
        assert!(short.contains("64 bytes"), "{short}");
        let garbage = CdpApiKey::parse(KEY_ID, "not-a-key!").unwrap_err();
        assert!(
            garbage.contains("PEM EC key or a base64 Ed25519 key"),
            "{garbage}"
        );
        let bad_pem = CdpApiKey::parse(
            KEY_ID,
            "-----BEGIN RSA PRIVATE KEY-----\nAAAA\n-----END RSA PRIVATE KEY-----",
        )
        .unwrap_err();
        assert!(bad_pem.contains("EC PRIVATE KEY"), "{bad_pem}");
        let mut pair = ed25519_dalek::SigningKey::from_bytes(&[0x22; 32]).to_keypair_bytes();
        pair[40] ^= 1;
        let mismatch = CdpApiKey::parse(KEY_ID, &STANDARD.encode(pair)).unwrap_err();
        assert!(mismatch.contains("does not match"), "{mismatch}");
        assert!(CdpApiKey::parse(" ", &ed25519_secret()).is_err());
    }

    #[test]
    fn debug_never_prints_the_secret() {
        let key = CdpApiKey::parse(KEY_ID, &ed25519_secret()).unwrap();
        let printed = format!("{key:?}");
        assert!(printed.contains("<redacted>"));
        assert!(!printed.contains(&ed25519_secret()));
    }

    #[test]
    fn facilitator_auth_binds_host_and_full_operation_path() {
        let key = CdpApiKey::parse(KEY_ID, &ec_pem()).unwrap();
        let auth =
            CdpFacilitatorAuth::new(key, "https://api.cdp.coinbase.com/platform/v2/x402/").unwrap();
        assert_eq!(auth.host(), CDP_API_HOST);
        assert_eq!(auth.full_path("/settle"), "/platform/v2/x402/settle");
        let headers = auth.headers("POST", "/settle");
        let bearer = &headers
            .iter()
            .find(|(k, _)| k == "authorization")
            .unwrap()
            .1;
        let jwt = bearer.strip_prefix("Bearer ").unwrap();
        let claims = decode_part(jwt.split('.').nth(1).unwrap());
        assert_eq!(
            claims["uris"][0],
            "POST api.cdp.coinbase.com/platform/v2/x402/settle"
        );
        let now = now_ms() / 1000;
        assert!((claims["nbf"].as_i64().unwrap() - now).abs() <= 5);
        assert!(headers.iter().any(|(k, _)| k == "correlation-context"));
        // Two calls never share a nonce.
        let again = auth.headers("POST", "/settle");
        assert_ne!(headers[0].1, again[0].1);
    }

    #[test]
    fn split_url_handles_ports_and_bare_hosts() {
        assert_eq!(
            split_url("http://localhost:8080").unwrap(),
            ("localhost:8080".to_string(), String::new())
        );
        assert!(is_cdp_url(CDP_FACILITATOR_URL));
        assert!(!is_cdp_url("https://x402.org/facilitator"));
        assert!(split_url("ftp://x").is_err());
    }
}
