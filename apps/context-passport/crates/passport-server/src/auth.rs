//! Constant-time `X-API-Key` check, fail-closed on a missing or empty key.

use axum::http::HeaderMap;
use subtle::ConstantTimeEq;

use crate::error::ApiError;
use passport_core::PassportError;

pub fn authorize(api_key: &str, headers: &HeaderMap) -> Result<(), ApiError> {
    let presented = headers
        .get("X-API-Key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let expected = api_key.as_bytes();
    // `ct_eq` returns 0 for unequal lengths, then compares bytes with no
    // early-exit timing leak on the shared prefix.
    let matches: bool = expected.ct_eq(presented.as_bytes()).into();
    if matches && !expected.is_empty() {
        Ok(())
    } else {
        Err(ApiError(PassportError::Unauthorized))
    }
}
