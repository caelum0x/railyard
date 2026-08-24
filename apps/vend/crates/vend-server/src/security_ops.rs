//! Security-operation policy shared by customer authentication and the data
//! plane. Counters are durable in Postgres; this module only derives
//! domain-separated, peppered subject digests and renders stable 429 responses.

use std::sync::Arc;

use axum::http::{header::RETRY_AFTER, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use vend_core::error::VendError;
use vend_store::{RateLimitRequest, SecurityOpsRepo};

#[derive(Clone, Copy, Debug)]
pub struct RateLimitPolicy {
    pub window_seconds: i32,
    pub max_attempts: i32,
    pub block_seconds: i32,
}

impl RateLimitPolicy {
    pub fn new(
        window_seconds: i32,
        max_attempts: i32,
        block_seconds: i32,
    ) -> Result<Self, VendError> {
        if !(1..=86_400).contains(&window_seconds)
            || !(1..=10_000).contains(&max_attempts)
            || !(1..=86_400).contains(&block_seconds)
        {
            return Err(VendError::Config("invalid rate-limit policy".into()));
        }
        Ok(Self {
            window_seconds,
            max_attempts,
            block_seconds,
        })
    }
}

#[derive(Clone, Debug)]
pub struct SecurityOpsConfig {
    pepper: [u8; 32],
    pub native_login: RateLimitPolicy,
    pub setup_token: RateLimitPolicy,
    pub oauth_start: RateLimitPolicy,
    pub oauth_callback: RateLimitPolicy,
    pub provisioning_key: RateLimitPolicy,
    pub cleanup_limit: i64,
}

impl SecurityOpsConfig {
    /// `pepper_hex` is a dedicated 256-bit runtime secret, not a bearer token
    /// and not shared with any OAuth or administrator credential.
    pub fn from_pepper_hex(pepper_hex: &str) -> Result<Self, VendError> {
        let pepper = parse_nonzero_hex_32(pepper_hex, "VEND_RATE_LIMIT_PEPPER_HEX")?;
        Ok(Self {
            pepper,
            native_login: RateLimitPolicy::new(60, 5, 300)?,
            setup_token: RateLimitPolicy::new(60, 5, 600)?,
            oauth_start: RateLimitPolicy::new(60, 20, 300)?,
            oauth_callback: RateLimitPolicy::new(60, 10, 600)?,
            provisioning_key: RateLimitPolicy::new(60, 10, 600)?,
            cleanup_limit: 100,
        })
    }

    pub fn subject_hash(&self, bucket: &str, tenant_scope: &str, subject: &str) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"vend-rate-limit-v1\0");
        digest.update(self.pepper);
        update_len_prefixed(&mut digest, bucket.as_bytes());
        update_len_prefixed(&mut digest, tenant_scope.as_bytes());
        update_len_prefixed(&mut digest, subject.as_bytes());
        digest.finalize().into()
    }
}

pub async fn enforce_rate_limit(
    repo: &Arc<dyn SecurityOpsRepo>,
    config: &SecurityOpsConfig,
    bucket: &str,
    tenant_scope: &str,
    subject: &str,
    policy: RateLimitPolicy,
) -> Result<(), Response> {
    let subject_hash = config.subject_hash(bucket, tenant_scope, subject);
    let decision = repo
        .check_rate_limit(RateLimitRequest {
            bucket: bucket.to_string(),
            tenant_scope: tenant_scope.to_string(),
            subject_hash,
            window_seconds: policy.window_seconds,
            max_attempts: policy.max_attempts,
            block_seconds: policy.block_seconds,
            cleanup_limit: config.cleanup_limit,
        })
        .await
        .map_err(internal_security_error)?;
    if decision.allowed {
        Ok(())
    } else {
        Err(rate_limited(decision.retry_after_seconds))
    }
}

pub async fn clear_rate_limit(
    repo: &Arc<dyn SecurityOpsRepo>,
    config: &SecurityOpsConfig,
    bucket: &str,
    tenant_scope: &str,
    subject: &str,
) -> Result<(), VendError> {
    repo.clear_rate_limit(
        bucket,
        tenant_scope,
        config.subject_hash(bucket, tenant_scope, subject),
    )
    .await
}

pub fn rate_limited(retry_after_seconds: u64) -> Response {
    let retry = retry_after_seconds.max(1);
    let mut response = (
        StatusCode::TOO_MANY_REQUESTS,
        Json(json!({
            "success": false,
            "error": {
                "code": "rate_limited",
                "message": "too many attempts; retry later"
            }
        })),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&retry.to_string()) {
        response.headers_mut().insert(RETRY_AFTER, value);
    }
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        HeaderValue::from_static("no-store"),
    );
    response
}

fn internal_security_error(error: VendError) -> Response {
    tracing::error!(%error, "security operation failed");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({
            "success": false,
            "error": {"code": "internal_error", "message": "internal error"}
        })),
    )
        .into_response()
}

fn update_len_prefixed(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

fn parse_nonzero_hex_32(value: &str, name: &str) -> Result<[u8; 32], VendError> {
    let value = value.trim();
    if value.len() != 64 {
        return Err(VendError::Config(format!(
            "{name} must be a 64-character hexadecimal secret"
        )));
    }
    let mut result = [0_u8; 32];
    for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
        result[index] = (hex_digit(pair[0]).ok_or_else(|| invalid_hex(name))? << 4)
            | hex_digit(pair[1]).ok_or_else(|| invalid_hex(name))?;
    }
    if result.ct_eq(&[0; 32]).unwrap_u8() == 1 {
        return Err(invalid_hex(name));
    }
    Ok(result)
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn invalid_hex(name: &str) -> VendError {
    VendError::Config(format!("{name} must contain non-zero hexadecimal bytes"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> SecurityOpsConfig {
        SecurityOpsConfig::from_pepper_hex(
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .unwrap()
    }

    #[test]
    fn subject_digest_is_stable_and_domain_separated() {
        let config = config();
        let first = config.subject_hash("native_login", "ten_a", "user@example.test");
        assert_eq!(
            first,
            config.subject_hash("native_login", "ten_a", "user@example.test")
        );
        assert_ne!(
            first,
            config.subject_hash("setup_token", "ten_a", "user@example.test")
        );
        assert_ne!(
            first,
            config.subject_hash("native_login", "ten_b", "user@example.test")
        );
    }

    #[test]
    fn rejects_weak_or_malformed_peppers() {
        assert!(SecurityOpsConfig::from_pepper_hex("00").is_err());
        assert!(SecurityOpsConfig::from_pepper_hex(&"0".repeat(64)).is_err());
        assert!(SecurityOpsConfig::from_pepper_hex(&"g".repeat(64)).is_err());
    }

    #[test]
    fn rate_limited_response_has_retry_after_and_no_store() {
        let response = rate_limited(17);
        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()[RETRY_AFTER], "17");
        assert_eq!(
            response.headers()[axum::http::header::CACHE_CONTROL],
            "no-store"
        );
    }
}
