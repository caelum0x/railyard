//! Outbound, Standard Webhooks signed event deliveries (Wave P PY1.5): Mint
//! telling a product endpoint about a mirrored paid event. The signature is
//! `v1,<base64 HMAC-SHA256(key, "id.timestamp.body")>` where the key is the
//! base64 part of a `whsec_` secret, as every product already verifies for
//! its payment provider.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::http::{HttpTransport, TransportError};

/// `v1,<signature>` for one delivery, or `None` when the secret is not a
/// base64 `whsec_` secret.
pub fn sign(secret: &str, id: &str, timestamp: &str, body: &[u8]) -> Option<String> {
    let raw = secret.trim();
    let key = STANDARD.decode(raw.strip_prefix("whsec_").unwrap_or(raw)).ok().filter(|k| !k.is_empty())?;
    let mut mac = Hmac::<Sha256>::new_from_slice(&key).ok()?;
    mac.update(id.as_bytes());
    mac.update(b".");
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    Some(format!("v1,{}", STANDARD.encode(mac.finalize().into_bytes())))
}

/// Posts one signed delivery; returns the HTTP status.
pub async fn deliver(
    transport: &dyn HttpTransport,
    url: &str,
    secret: &str,
    id: &str,
    timestamp: &str,
    body: &[u8],
) -> Result<u16, TransportError> {
    let signature = sign(secret, id, timestamp, body).ok_or_else(|| TransportError("the endpoint secret is not a whsec_ secret".to_owned()))?;
    let headers = vec![
        ("webhook-id".to_owned(), id.to_owned()),
        ("webhook-timestamp".to_owned(), timestamp.to_owned()),
        ("webhook-signature".to_owned(), signature),
    ];
    Ok(transport.post_json(url, &headers, body).await?.status)
}

#[cfg(test)]
mod tests {
    use super::sign;

    #[test]
    fn signs_like_standard_webhooks() {
        // The same vector the Railyard bridge contract pins (railyard
        // tests/contract/fixtures/scribase-bridge/contract.json).
        let secret = "whsec_Y29udHJhY3QtZml4dHVyZS1zZWNyZXQtMzItYnl0ZXM=";
        let body = br#"{"scribase_org_id":"org_fixture","slot":"deploy"}"#;
        assert_eq!(
            sign(secret, "msg_vector_1", "1790683200", body).as_deref(),
            Some("v1,2STo0Y/k7q3ba+G8+nDgJbNv/m3d9AefmzC0Hzrbjjw=")
        );
        assert_eq!(sign("whsec_!!", "a", "1", b"{}"), None);
    }
}
