//! Reusable webhook signature families. Every function operates on RAW bytes
//! BEFORE any serde parse, and every comparison is constant-time (via
//! `Mac::verify_slice` or [`subtle::ConstantTimeEq`]) so a byte-flip in the
//! signature is rejected without leaking timing.
//!
//! - [`hmac_sha256_hex_verify`] — Stripe / Creem / Coinbase Commerce.
//! - [`standard_webhooks_verify`] — Polar (Standard Webhooks base64 scheme).
//! - [`const_eq_secret`] — RevenueCat / Adapty `Authorization` equality.
//! - [`iyzws_v2_sign`] — Iyzico IYZWSv2 outbound auth header.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::event::WebhookError;

type HmacSha256 = Hmac<Sha256>;

/// Verify a lowercase-hex HMAC-SHA256 signature over `message` with `secret`.
/// Used by Stripe (`message = "{t}.{body}"`) and Creem / Coinbase Commerce
/// (`message = raw body`). Empty secret ⇒ [`WebhookError::NotConfigured`].
pub fn hmac_sha256_hex_verify(
    secret: &[u8],
    message: &[u8],
    provided_hex: &str,
) -> Result<(), WebhookError> {
    if secret.is_empty() {
        return Err(WebhookError::NotConfigured);
    }
    let expected = hex::decode(provided_hex.trim()).map_err(|_| WebhookError::BadSignature)?;
    let mut mac = HmacSha256::new_from_slice(secret).map_err(|_| WebhookError::NotConfigured)?;
    mac.update(message);
    mac.verify_slice(&expected)
        .map_err(|_| WebhookError::BadSignature)
}

/// Compute the raw HMAC-SHA256 tag of `message` under `secret` (helper for the
/// base64/Standard-Webhooks family and for test signing).
pub fn hmac_sha256_raw(secret: &[u8], message: &[u8]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(secret).expect("hmac accepts any key length");
    mac.update(message);
    mac.finalize().into_bytes().into()
}

/// Verify a [Standard Webhooks](https://www.standardwebhooks.com/) signature
/// (Polar): the signed content is `"{id}.{ts}.{body}"`, the secret is
/// `whsec_<base64>` (the `whsec_` prefix is stripped and the remainder base64
/// decoded to the raw key), and the `webhook-signature` header is a
/// space-separated list of `v1,<base64tag>` entries — any matching entry passes.
pub fn standard_webhooks_verify(
    secret: &str,
    id: &str,
    ts: &str,
    body: &[u8],
    sig_header: &str,
) -> Result<(), WebhookError> {
    if secret.is_empty() {
        return Err(WebhookError::NotConfigured);
    }
    let key_b64 = secret.strip_prefix("whsec_").unwrap_or(secret);
    let key = B64
        .decode(key_b64)
        .map_err(|_| WebhookError::NotConfigured)?;

    let mut signed = Vec::with_capacity(id.len() + ts.len() + body.len() + 2);
    signed.extend_from_slice(id.as_bytes());
    signed.push(b'.');
    signed.extend_from_slice(ts.as_bytes());
    signed.push(b'.');
    signed.extend_from_slice(body);
    let expected = hmac_sha256_raw(&key, &signed);

    for part in sig_header.split_whitespace() {
        let b64 = part.strip_prefix("v1,").unwrap_or(part);
        if let Ok(provided) = B64.decode(b64) {
            if const_eq_secret(&expected, &provided) {
                return Ok(());
            }
        }
    }
    Err(WebhookError::BadSignature)
}

/// Constant-time byte equality (differing lengths ⇒ `false`). Used for the
/// RevenueCat / Adapty `Authorization` shared-secret comparison — NOT an HMAC.
pub fn const_eq_secret(expected: &[u8], presented: &[u8]) -> bool {
    if expected.len() != presented.len() {
        return false;
    }
    expected.ct_eq(presented).into()
}

/// Build an Iyzico **IYZWSv2** `Authorization` header value for an outbound API
/// call. The signature is `hex(HMAC-SHA256(secret_key, random_key + uri_path +
/// request_body))`; the header is `"IYZWSv2 " + base64("apiKey:{api_key}&
/// randomKey:{random_key}&signature:{sig_hex}")`.
pub fn iyzws_v2_sign(
    api_key: &str,
    secret_key: &str,
    random_key: &str,
    uri_path: &str,
    request_body: &[u8],
) -> String {
    let mut payload = Vec::new();
    payload.extend_from_slice(random_key.as_bytes());
    payload.extend_from_slice(uri_path.as_bytes());
    payload.extend_from_slice(request_body);
    let sig = hmac_sha256_raw(secret_key.as_bytes(), &payload);
    let sig_hex = hex::encode(sig);
    let authz = format!("apiKey:{api_key}&randomKey:{random_key}&signature:{sig_hex}");
    format!("IYZWSv2 {}", B64.encode(authz.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmac_hex_round_trips_and_rejects_flip() {
        let secret = b"whsec_test";
        let body = br#"{"ok":true}"#;
        let sig = hex::encode(hmac_sha256_raw(secret, body));
        assert!(hmac_sha256_hex_verify(secret, body, &sig).is_ok());

        // Flip one hex nibble → BadSignature.
        let mut bad = sig.clone();
        let last = bad.pop().unwrap();
        bad.push(if last == 'a' { 'b' } else { 'a' });
        assert_eq!(
            hmac_sha256_hex_verify(secret, body, &bad),
            Err(WebhookError::BadSignature)
        );
    }

    #[test]
    fn empty_secret_is_not_configured() {
        assert_eq!(
            hmac_sha256_hex_verify(b"", b"x", "00"),
            Err(WebhookError::NotConfigured)
        );
    }

    #[test]
    fn standard_webhooks_round_trip() {
        let raw_key = b"super-secret-key";
        let secret = format!("whsec_{}", B64.encode(raw_key));
        let id = "msg_1";
        let ts = "1700000000";
        let body = br#"{"type":"order.paid"}"#;
        let mut signed = Vec::new();
        signed.extend_from_slice(id.as_bytes());
        signed.push(b'.');
        signed.extend_from_slice(ts.as_bytes());
        signed.push(b'.');
        signed.extend_from_slice(body);
        let tag = B64.encode(hmac_sha256_raw(raw_key, &signed));
        let header = format!("v1,{tag}");
        assert!(standard_webhooks_verify(&secret, id, ts, body, &header).is_ok());
        // Wrong timestamp → BadSignature.
        assert_eq!(
            standard_webhooks_verify(&secret, id, "1700000001", body, &header),
            Err(WebhookError::BadSignature)
        );
    }

    #[test]
    fn const_eq_is_length_and_value_sensitive() {
        assert!(const_eq_secret(b"abc", b"abc"));
        assert!(!const_eq_secret(b"abc", b"abd"));
        assert!(!const_eq_secret(b"abc", b"abcd"));
    }

    #[test]
    fn iyzws_v2_is_deterministic() {
        let a = iyzws_v2_sign("k", "s", "r", "/payment/detail", b"{}");
        let b = iyzws_v2_sign("k", "s", "r", "/payment/detail", b"{}");
        assert_eq!(a, b);
        assert!(a.starts_with("IYZWSv2 "));
    }
}
