//! Authentication primitives for the dashboard admin surface: argon2 password
//! hashing (PHC strings; plaintext is never stored) and HS256 JWT issuance
//! matching Lago's `{ token, user }` login payload shape.
//!
//! The JWT signing secret is read from `RAGO_JWT_SECRET`. Release builds
//! refuse to run without it (call [`validate_jwt_config`] at boot) — the
//! well-known development fallback is only used in debug builds or behind an
//! explicit `RAGO_DEV_INSECURE_JWT=1` opt-in, so a default production deploy
//! can never accept attacker-forged admin tokens.

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

/// The well-known development-only signing secret. Anyone can forge tokens
/// under it, so it must never be reachable on a production deploy.
const DEV_FALLBACK_SECRET: &str = "rago-dev-jwt-secret";

/// Pure secret resolution — testable without touching process env.
/// `dev_fallback_allowed` is true for debug builds or the explicit
/// `RAGO_DEV_INSECURE_JWT=1` opt-in.
fn resolve_jwt_secret(env_secret: Option<String>, dev_fallback_allowed: bool) -> Result<String, String> {
    match env_secret {
        Some(s) if !s.trim().is_empty() => Ok(s),
        _ if dev_fallback_allowed => Ok(DEV_FALLBACK_SECRET.to_string()),
        _ => Err(
            "RAGO_JWT_SECRET is not set — refusing to fall back to the insecure development \
             secret. Set RAGO_JWT_SECRET (e.g. `openssl rand -hex 32`), or set \
             RAGO_DEV_INSECURE_JWT=1 to explicitly opt into the dev-only fallback."
                .to_string(),
        ),
    }
}

fn dev_fallback_allowed() -> bool {
    cfg!(debug_assertions) || std::env::var("RAGO_DEV_INSECURE_JWT").as_deref() == Ok("1")
}

/// Fail-fast configuration check for server startup: errors when no usable
/// JWT secret is configured (release build, no `RAGO_JWT_SECRET`, no dev
/// opt-in) so a misconfigured deploy refuses to boot instead of silently
/// accepting forgeable tokens.
pub fn validate_jwt_config() -> Result<(), String> {
    resolve_jwt_secret(std::env::var("RAGO_JWT_SECRET").ok(), dev_fallback_allowed()).map(|_| ())
}

fn jwt_secret() -> String {
    // Backstop for callers that bypassed `validate_jwt_config`: refuse to
    // sign/verify with the insecure fallback outside dev.
    resolve_jwt_secret(std::env::var("RAGO_JWT_SECRET").ok(), dev_fallback_allowed())
        .unwrap_or_else(|e| panic!("{e}"))
}

fn b64url(data: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(data)
}

/// Issue a signed HS256 JWT for a user session (24h expiry), matching Lago's
/// token shape (`sub`, `exp`, `login_method`).
pub fn issue_jwt(user_id: Uuid) -> String {
    sign_jwt(serde_json::json!({ "sub": user_id.to_string(), "login_method": "email_password" }))
}

/// Issue a session JWT for an SSO login, pinned to the organization whose
/// SSO integration authenticated the user (`org` claim) so a member of
/// several organizations lands in the right one.
pub fn issue_sso_jwt(user_id: Uuid, organization_id: Uuid, provider: &str) -> String {
    sign_jwt(serde_json::json!({
        "sub": user_id.to_string(),
        "org": organization_id.to_string(),
        "login_method": provider,
    }))
}

fn sign_jwt(mut claims: serde_json::Value) -> String {
    let header = b64url(br#"{"alg":"HS256","typ":"JWT"}"#);
    let exp = (Utc::now() + Duration::days(1)).timestamp();
    claims["exp"] = serde_json::json!(exp);
    let payload = b64url(claims.to_string().as_bytes());
    let signing_input = format!("{header}.{payload}");
    let mut mac = Hmac::<Sha256>::new_from_slice(jwt_secret().as_bytes()).expect("HMAC accepts any key length");
    mac.update(signing_input.as_bytes());
    let sig = b64url(&mac.finalize().into_bytes());
    format!("{signing_input}.{sig}")
}

/// Verify a JWT's signature and expiry, returning the `sub` (user id) on success.
pub fn verify_jwt(token: &str) -> Option<Uuid> {
    verify_jwt_claims(token).map(|(user, _)| user)
}

/// Verify a JWT and return `(user_id, pinned organization_id)`.
pub fn verify_jwt_claims(token: &str) -> Option<(Uuid, Option<Uuid>)> {
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
    let user = json.get("sub").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok())?;
    let org = json.get("org").and_then(|v| v.as_str()).and_then(|s| Uuid::parse_str(s).ok());
    Some((user, org))
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

    #[test]
    fn secret_from_env_always_wins() {
        assert_eq!(resolve_jwt_secret(Some("prod-secret".into()), false), Ok("prod-secret".into()));
        assert_eq!(resolve_jwt_secret(Some("prod-secret".into()), true), Ok("prod-secret".into()));
    }

    #[test]
    fn missing_secret_falls_back_only_with_dev_opt_in() {
        assert_eq!(resolve_jwt_secret(None, true), Ok(DEV_FALLBACK_SECRET.into()));
        assert!(resolve_jwt_secret(None, false).is_err());
    }

    #[test]
    fn blank_secret_is_treated_as_missing() {
        assert!(resolve_jwt_secret(Some("   ".into()), false).is_err());
        assert_eq!(resolve_jwt_secret(Some(String::new()), true), Ok(DEV_FALLBACK_SECRET.into()));
    }
}
