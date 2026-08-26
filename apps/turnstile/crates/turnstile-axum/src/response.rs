//! `402` and receipt responses built from the reused AITP-402 codec.
//!
//! Header names are emitted lowercase (HTTP headers are case-insensitive, and
//! `http::HeaderName::from_static` requires lowercase); wire semantics are
//! unchanged from the `AITP-*` names in [`aitp_client::codec`].

use aitp_client::codec::{encode_challenge, encode_receipt, AITP_VERSION};
use aitp_client::{PaymentChallenge, Receipt};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use turnstile_core::TurnstileError;

const H_CHALLENGE: &str = "aitp-challenge";
const H_VERSION: &str = "aitp-version";
const H_RECEIPT: &str = "aitp-receipt";
const H_ERROR: &str = "aitp-error";

fn header_name(name: &'static str) -> HeaderName {
    HeaderName::from_static(name)
}

/// A `402 Payment Required` carrying the challenge and protocol version.
pub fn challenge(challenge: &PaymentChallenge) -> Response {
    let mut resp = (StatusCode::PAYMENT_REQUIRED, "payment required\n").into_response();
    let headers = resp.headers_mut();
    if let Ok(encoded) = encode_challenge(challenge) {
        if let Ok(value) = HeaderValue::from_str(&encoded) {
            headers.insert(header_name(H_CHALLENGE), value);
        }
    }
    headers.insert(
        header_name(H_VERSION),
        HeaderValue::from_static(AITP_VERSION),
    );
    resp
}

/// A `402` for a present-but-invalid payment: re-challenge with a fresh nonce and
/// surface the rejection reason in an `AITP-Error` header (best effort).
pub fn rejected(error: &TurnstileError, fresh: &PaymentChallenge) -> Response {
    let mut resp = challenge(fresh);
    if let Ok(value) = HeaderValue::from_str(&error.to_string()) {
        resp.headers_mut().insert(header_name(H_ERROR), value);
    }
    resp
}

/// A `402` when we could not even mint a fresh challenge (e.g. the durable nonce
/// store is unavailable). Fails closed: surfaces the reason in `AITP-Error` and
/// carries no `AITP-Challenge`, so the client cannot proceed without payment.
pub fn unavailable(error: &TurnstileError) -> Response {
    let mut resp = (StatusCode::PAYMENT_REQUIRED, "payment required\n").into_response();
    let headers = resp.headers_mut();
    if let Ok(value) = HeaderValue::from_str(&error.to_string()) {
        headers.insert(header_name(H_ERROR), value);
    }
    headers.insert(
        header_name(H_VERSION),
        HeaderValue::from_static(AITP_VERSION),
    );
    resp
}

/// Attach an `AITP-Receipt` (when a capture produced one) and the protocol
/// version to a successful handler response.
pub fn attach_receipt(mut resp: Response, receipt: Option<Receipt>) -> Response {
    let headers = resp.headers_mut();
    if let Some(receipt) = receipt {
        if let Ok(encoded) = encode_receipt(&receipt) {
            if let Ok(value) = HeaderValue::from_str(&encoded) {
                headers.insert(header_name(H_RECEIPT), value);
            }
        }
    }
    headers.insert(
        header_name(H_VERSION),
        HeaderValue::from_static(AITP_VERSION),
    );
    resp
}
