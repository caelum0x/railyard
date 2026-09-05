//! Authentication primitives for the dashboard admin surface: argon2 password
//! hashing (PHC strings; plaintext is never stored) and HS256 JWT issuance
//! matching Lago's `{ token, user }` login payload shape.
//!
//! The JWT signing secret is read from `RAGO_JWT_SECRET` (falling back to a
//! development constant) — never hardcode a production secret.

use argon2::password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use base64::Engine;
use chrono::{Duration, Utc};
use hmac::{Hmac, Mac};
use sha2::Sha256;
use uuid::Uuid;

/// Hash a password with argon2id, returning the PHC-format digest to persist.
pub fn hash_password(password: &str) -> Result<String, String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| e.to_string())
}

/// Verify a password against a stored argon2 digest (constant-time within
/// argon2). Returns `false` on any parse/verify failure — never panics.
pub fn verify_password(password: &str, digest: &str) -> bool {
    match PasswordHash::new(digest) {
        Ok(parsed) => Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok(),
        Err(_) => false,
    }
}

fn jwt_secret() -> String {
    std::env::var("RAGO_JWT_SECRET").unwrap_or_else(|_| "rago-dev-jwt-secret".to_string())
}

fn b64url(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

/// Issue a signed HS256 JWT for a user session (24h expiry), matching Lago's
/// token shape (`sub`, `exp`, `login_method`).
pub fn issue_jwt(user_id: Uuid) -> String {
    let header = b64url(br#"{"alg":"HS256","typ":"JWT"}"#);
    let exp = (Utc::now() + Duration::days(1)).timestamp();
    let payload = b64url(
        serde_json::json!({ "sub": user_id.to_string(), "exp": exp, "login_method": "email_password" })
            .to_string()
            .as_bytes(),
    );
    let signing_input = format!("{header}.{payload}");
    let mut mac = Hmac::<Sha256>::new_from_slice(jwt_secret().as_bytes()).expect("HMAC accepts any key length");
    mac.update(signing_input.as_bytes());
    let sig = b64url(&mac.finalize().into_bytes());
    format!("{signing_input}.{sig}")
}

/// Verify a JWT's signature and expiry, returning the `sub` (user id) on success.
pub fn verify_jwt(token: &str) -> Option<Uuid> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return None;
    }
    let signing_input = format!("{}.{}", parts[0], parts[1]);
    let mut mac = Hmac::<Sha256>::new_from_slice(jwt_secret().as_bytes()).ok()?;
    mac.update(signing_input.as_bytes());
    let expected = b64url(&mac.finalize().into_bytes());
    if expected != parts[2] {
        return None;
    }
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(parts[1]).ok()?;
    let json: serde_json::Value = serde_json::from_slice(&payload).ok()?;
    let exp = json.get("exp").and_then(|v| v.as_i64())?;
    if Utc::now().timestamp() > exp {
        return None;
    }
    json.get("sub").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok())
}

/// Generate an opaque, URL-safe token (invites, password resets).
pub fn random_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn password_hash_roundtrip() {
        let digest = hash_password("s3cret!").unwrap();
        assert!(digest.starts_with("$argon2"));
        assert!(verify_password("s3cret!", &digest));
        assert!(!verify_password("wrong", &digest));
    }

    #[test]
    fn jwt_roundtrip() {
        let uid = Uuid::new_v4();
        let token = issue_jwt(uid);
        assert_eq!(verify_jwt(&token), Some(uid));
        assert!(verify_jwt("garbage.token.here").is_none());
    }
}
