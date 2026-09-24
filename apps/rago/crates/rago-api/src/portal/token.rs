//! Signed, expiring customer-portal tokens.
//!
//! `v1.<base64url(claims JSON)>.<base64url(HMAC-SHA256)>`. The MAC key is
//! derived from the organization's signing key (`organizations.hmac_key`) with
//! a portal-specific label, so a portal token can never be confused with an
//! outbound-webhook signature and a token minted for one organization never
//! verifies under another's key. Each token names a `customer_portal_sessions`
//! row, which makes it revocable; the expiry is enforced both here and by the
//! row.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const VERSION: &str = "v1";
const KEY_LABEL: &[u8] = b"rago-customer-portal-v1";

/// What a portal token asserts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortalClaims {
    /// `customer_portal_sessions.id`.
    #[serde(rename = "sid")]
    pub session_id: Uuid,
    #[serde(rename = "org")]
    pub organization_id: Uuid,
    #[serde(rename = "cus")]
    pub customer_id: Uuid,
    /// Expiry, unix seconds.
    #[serde(rename = "exp")]
    pub expires_at: i64,
}

/// Why a token was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    Malformed,
    BadSignature,
    Expired,
}

/// The portal MAC key for an organization signing key.
pub fn derive_key(org_signing_key: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(KEY_LABEL);
    h.update([0u8]);
    h.update(org_signing_key.as_bytes());
    h.finalize().into()
}

fn mac(key: &[u8; 32], signed: &str) -> Vec<u8> {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("HMAC any key size");
    m.update(signed.as_bytes());
    m.finalize().into_bytes().to_vec()
}

/// Mint a token for `claims` under the organization key.
pub fn sign(claims: &PortalClaims, key: &[u8; 32]) -> String {
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).expect("claims serialize"));
    let signed = format!("{VERSION}.{payload}");
    let sig = URL_SAFE_NO_PAD.encode(mac(key, &signed));
    format!("{signed}.{sig}")
}

/// Read the claims WITHOUT verifying them (to find which organization's key
/// verifies the token). Never trust the result before [`verify`].
pub fn peek(token: &str) -> Result<PortalClaims, TokenError> {
    let mut parts = token.split('.');
    let (Some(VERSION), Some(payload), Some(_sig), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(TokenError::Malformed);
    };
    let raw = URL_SAFE_NO_PAD.decode(payload).map_err(|_| TokenError::Malformed)?;
    serde_json::from_slice(&raw).map_err(|_| TokenError::Malformed)
}

/// Verify the signature under `key` and the expiry at `now` (unix seconds).
pub fn verify(token: &str, key: &[u8; 32], now: i64) -> Result<PortalClaims, TokenError> {
    let claims = peek(token)?;
    let (signed, sig) = token.rsplit_once('.').ok_or(TokenError::Malformed)?;
    let provided = URL_SAFE_NO_PAD.decode(sig).map_err(|_| TokenError::Malformed)?;
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("HMAC any key size");
    m.update(signed.as_bytes());
    m.verify_slice(&provided).map_err(|_| TokenError::BadSignature)?;
    if now >= claims.expires_at {
        return Err(TokenError::Expired);
    }
    Ok(claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(exp: i64) -> PortalClaims {
        PortalClaims {
            session_id: Uuid::from_u128(1),
            organization_id: Uuid::from_u128(2),
            customer_id: Uuid::from_u128(3),
            expires_at: exp,
        }
    }

    #[test]
    fn round_trips_until_expiry() {
        let key = derive_key("org-a-signing-key");
        let token = sign(&claims(1_000), &key);
        assert_eq!(verify(&token, &key, 999), Ok(claims(1_000)));
        assert_eq!(verify(&token, &key, 1_000), Err(TokenError::Expired));
    }

    #[test]
    fn another_organizations_key_or_tampering_is_rejected() {
        let key = derive_key("org-a-signing-key");
        let other = derive_key("org-b-signing-key");
        let token = sign(&claims(1_000), &key);
        assert_eq!(verify(&token, &other, 1), Err(TokenError::BadSignature));

        // Swap in claims for another customer, keeping the original signature.
        let (_, sig) = token.rsplit_once('.').unwrap();
        let mut forged_claims = claims(1_000);
        forged_claims.customer_id = Uuid::from_u128(99);
        let forged_payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged_claims).unwrap());
        let forged = format!("v1.{forged_payload}.{sig}");
        assert_eq!(verify(&forged, &key, 1), Err(TokenError::BadSignature));

        assert_eq!(verify("v1.x", &key, 1), Err(TokenError::Malformed));
        assert_eq!(verify("v2.a.b", &key, 1), Err(TokenError::Malformed));
    }

    #[test]
    fn portal_key_differs_from_the_raw_signing_key() {
        let key = derive_key("k");
        assert_ne!(&key[..], Sha256::digest(b"k").as_slice());
    }
}
