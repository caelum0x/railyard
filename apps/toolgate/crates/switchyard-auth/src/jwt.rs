//! EdDSA (Ed25519) access tokens, compact JWS.

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::json;
use switchyard_core::ApiError;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AccessClaims {
    pub iss: String,
    /// User id.
    pub sub: String,
    /// The resource (audience) this token is bound to.
    pub aud: String,
    pub exp: i64,
    pub iat: i64,
    pub jti: String,
    pub org: String,
    pub prj: String,
    /// Space-separated scopes.
    pub scope: String,
    /// Grant family (consents and revocation are per family).
    pub fam: String,
    /// OAuth client id.
    pub cid: String,
}

impl AccessClaims {
    pub fn scopes(&self) -> Vec<String> {
        self.scope.split(' ').filter(|s| !s.is_empty()).map(str::to_string).collect()
    }
}

pub struct TokenSigner {
    key: SigningKey,
    kid: String,
}

impl TokenSigner {
    /// From a 32-byte seed (base64); `None` generates a random key.
    pub fn new(seed_b64: Option<&str>) -> Result<Self, String> {
        let key = match seed_b64 {
            Some(s) => {
                let bytes = base64::engine::general_purpose::STANDARD.decode(s.trim()).map_err(|e| e.to_string())?;
                let seed: [u8; 32] = bytes.try_into().map_err(|_| "JWT seed must be 32 bytes".to_string())?;
                SigningKey::from_bytes(&seed)
            }
            None => SigningKey::generate(&mut rand::rngs::OsRng),
        };
        let kid = hex::encode(&key.verifying_key().to_bytes()[..8]);
        Ok(TokenSigner { key, kid })
    }

    pub fn verifying_key(&self) -> VerifyingKey {
        self.key.verifying_key()
    }

    /// The public JWKS (OKP / Ed25519).
    pub fn jwks(&self) -> serde_json::Value {
        json!({ "keys": [{ "kty": "OKP", "crv": "Ed25519", "alg": "EdDSA", "use": "sig", "kid": self.kid, "x": B64.encode(self.verifying_key().to_bytes()) }] })
    }

    pub fn sign(&self, claims: &AccessClaims) -> String {
        let header = json!({ "alg": "EdDSA", "typ": "at+jwt", "kid": self.kid });
        let h = B64.encode(header.to_string());
        let p = B64.encode(serde_json::to_vec(claims).expect("claims serialise"));
        let input = format!("{h}.{p}");
        let sig: Signature = self.key.sign(input.as_bytes());
        format!("{input}.{}", B64.encode(sig.to_bytes()))
    }

    /// Verify signature, expiry and audience.
    pub fn verify(&self, token: &str, audiences: &[&str], now: i64) -> Result<AccessClaims, ApiError> {
        let bad = || ApiError::unauthorized("invalid access token");
        let mut parts = token.split('.');
        let (Some(h), Some(p), Some(s), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
            return Err(bad());
        };
        let header: serde_json::Value = serde_json::from_slice(&B64.decode(h).map_err(|_| bad())?).map_err(|_| bad())?;
        if header.get("alg").and_then(|a| a.as_str()) != Some("EdDSA") {
            return Err(bad());
        }
        let sig_bytes: [u8; 64] = B64.decode(s).map_err(|_| bad())?.try_into().map_err(|_| bad())?;
        self.key.verifying_key().verify(format!("{h}.{p}").as_bytes(), &Signature::from_bytes(&sig_bytes)).map_err(|_| bad())?;
        let claims: AccessClaims = serde_json::from_slice(&B64.decode(p).map_err(|_| bad())?).map_err(|_| bad())?;
        if claims.exp <= now {
            return Err(ApiError::unauthorized("access token expired"));
        }
        if !audiences.contains(&claims.aud.as_str()) {
            return Err(ApiError::unauthorized("access token audience does not match this resource"));
        }
        Ok(claims)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(aud: &str, exp: i64) -> AccessClaims {
        AccessClaims {
            iss: "https://sy".into(),
            sub: "u1".into(),
            aud: aud.into(),
            exp,
            iat: 0,
            jti: "j".into(),
            org: "org_1".into(),
            prj: "prj_1".into(),
            scope: "tools:read tools:call".into(),
            fam: "f".into(),
            cid: "c".into(),
        }
    }

    #[test]
    fn sign_verify_audience_and_expiry() {
        let s = TokenSigner::new(None).unwrap();
        let t = s.sign(&claims("https://sy/mcp", 100));
        assert_eq!(s.verify(&t, &["https://sy/mcp"], 50).unwrap().scopes().len(), 2);
        assert!(s.verify(&t, &["https://other/mcp"], 50).is_err(), "wrong resource");
        assert!(s.verify(&t, &["https://sy/mcp"], 100).is_err(), "expired");
        let other = TokenSigner::new(None).unwrap();
        assert!(other.verify(&t, &["https://sy/mcp"], 50).is_err(), "foreign signer");
        let mut forged: Vec<&str> = t.split('.').collect();
        let evil = B64.encode(serde_json::to_vec(&claims("https://sy/mcp", 10_000)).unwrap());
        forged[1] = &evil;
        assert!(s.verify(&forged.join("."), &["https://sy/mcp"], 50).is_err(), "payload tamper");
    }

    #[test]
    fn deterministic_from_seed() {
        let seed = base64::engine::general_purpose::STANDARD.encode([7u8; 32]);
        let a = TokenSigner::new(Some(&seed)).unwrap();
        let b = TokenSigner::new(Some(&seed)).unwrap();
        assert_eq!(a.verifying_key(), b.verifying_key());
        assert_eq!(a.jwks()["keys"][0]["crv"], "Ed25519");
    }
}
