//! `402` and receipt responses built from the reused AITP-402 codec.
//!
//! Header names are emitted lowercase (HTTP headers are case-insensitive, and
//! `http::HeaderName::from_static` requires lowercase); wire semantics are
//! unchanged from the `AITP-*` names in [`aitp_client::codec`].

use aitp_client::codec::{encode_challenge, encode_receipt, AITP_VERSION};
use aitp_client::{PaymentChallenge, Receipt};
use axum::http::{HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use turnstile_core::x402::{
    PaymentRequiredBody, PaymentRequiredV2, SettleResponse, HEADER_PAYMENT_REQUIRED,
    HEADER_PAYMENT_RESPONSE, HEADER_PAYMENT_RESPONSE_V2, X402_VERSION_V2,
};
use turnstile_core::TurnstileError;

const H_CHALLENGE: &str = "aitp-challenge";
const H_VERSION: &str = "aitp-version";
const H_RECEIPT: &str = "aitp-receipt";
const H_REQUEST_ID: &str = "aitp-request-id";
const H_ERROR: &str = "aitp-error";
const H_SETTLEMENT: &str = "aitp-settlement";

fn header_name(name: &'static str) -> HeaderName {
    HeaderName::from_static(name)
}

/// A `402 Payment Required` carrying the challenge and protocol version.
pub fn challenge(challenge: &PaymentChallenge) -> Response {
    challenge_with(challenge, None)
}

/// Headers x402 clients must be able to read cross-origin.
const X402_EXPOSED: &str = "payment-required, payment-response, x-payment-response";

/// The `402` body: the x402 v1 `PaymentRequiredBody` JSON when x402 is enabled
/// (v1 clients read the body), plus the base64 v2 `PaymentRequired` in the
/// `PAYMENT-REQUIRED` header (v2 clients read the header); AITP clients read
/// the `AITP-*` headers. Without x402 the body is text.
fn payment_required(x402: Option<&PaymentRequiredBody>) -> Response {
    match x402 {
        Some(body) => {
            let mut resp = (StatusCode::PAYMENT_REQUIRED, axum::Json(body)).into_response();
            if let Some(value) = PaymentRequiredV2::from_v1(body)
                .and_then(|v2| turnstile_core::x402::wire::encode_header(&v2).ok())
                .and_then(|encoded| HeaderValue::from_str(&encoded).ok())
            {
                let headers = resp.headers_mut();
                headers.insert(HeaderName::from_static(HEADER_PAYMENT_REQUIRED), value);
                headers.insert(
                    HeaderName::from_static("access-control-expose-headers"),
                    HeaderValue::from_static(X402_EXPOSED),
                );
            }
            resp
        }
        None => (StatusCode::PAYMENT_REQUIRED, "payment required\n").into_response(),
    }
}

/// A `402` carrying the AITP challenge headers and, when given, the x402
/// `accepts[]` body — one response both protocols understand.
pub fn challenge_with(challenge: &PaymentChallenge, x402: Option<&PaymentRequiredBody>) -> Response {
    let mut resp = payment_required(x402);
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
    if let Ok(value) = HeaderValue::from_str(&challenge.nonce) {
        headers.insert(header_name(H_REQUEST_ID), value);
    }
    resp
}

/// A `402` for a present-but-invalid payment: re-challenge with a fresh nonce and
/// surface the rejection reason in an `AITP-Error` header (best effort).
pub fn rejected(error: &TurnstileError, fresh: &PaymentChallenge) -> Response {
    rejected_with(error, fresh, None)
}

/// [`rejected`] plus the x402 body.
pub fn rejected_with(
    error: &TurnstileError,
    fresh: &PaymentChallenge,
    x402: Option<&PaymentRequiredBody>,
) -> Response {
    let mut resp = challenge_with(fresh, x402);
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

/// Replace an application success with a truthful service failure when its
/// capture could not be confirmed. The durable journal will replay capture;
/// returning the handler's `2xx` here would falsely claim payment completion.
pub fn settlement_unavailable(error: &TurnstileError, request_id: Option<&str>) -> Response {
    let mut response = (
        StatusCode::SERVICE_UNAVAILABLE,
        "payment settlement pending\n",
    )
        .into_response();
    let headers = response.headers_mut();
    headers.insert(
        header_name(H_VERSION),
        HeaderValue::from_static(AITP_VERSION),
    );
    headers.insert(
        header_name(H_SETTLEMENT),
        HeaderValue::from_static("capture_pending"),
    );
    if let Ok(value) = HeaderValue::from_str(&error.to_string()) {
        headers.insert(header_name(H_ERROR), value);
    }
    attach_request_id(response, request_id)
}

/// Mark an existing application error when release is durable but not yet
/// confirmed by Mint. Recovery will only replay release.
pub fn mark_release_pending(mut response: Response, error: &TurnstileError) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header_name(H_VERSION),
        HeaderValue::from_static(AITP_VERSION),
    );
    headers.insert(
        header_name(H_SETTLEMENT),
        HeaderValue::from_static("release_pending"),
    );
    if let Ok(value) = HeaderValue::from_str(&error.to_string()) {
        headers.insert(header_name(H_ERROR), value);
    }
    response
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

/// Attach the request correlation derived from the authorization nonce.
pub fn attach_request_id(mut response: Response, request_id: Option<&str>) -> Response {
    if let Some(request_id) = request_id {
        if let Ok(value) = HeaderValue::from_str(request_id) {
            response
                .headers_mut()
                .insert(header_name(H_REQUEST_ID), value);
        }
    }
    response
}

/// A `402` for an `X-PAYMENT` that could not be accepted: the x402 body names
/// the reason in `error` and re-advertises `accepts[]`; a fresh AITP challenge
/// is attached when one could be issued so either rail can retry.
pub fn x402_rejected(body: &PaymentRequiredBody, fresh: Option<&PaymentChallenge>) -> Response {
    match fresh {
        Some(challenge) => challenge_with(challenge, Some(body)),
        None => {
            let mut resp = payment_required(Some(body));
            resp.headers_mut().insert(
                header_name(H_VERSION),
                HeaderValue::from_static(AITP_VERSION),
            );
            resp
        }
    }
}

/// A `503` when the x402 facilitator or journal is unavailable: not the
/// payer's fault, nothing was charged, safe to retry the same payment later
/// only if it was never reserved (the body says which).
pub fn x402_unavailable(code: &str, message: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        axum::Json(serde_json::json!({
            "x402Version": turnstile_core::x402::X402_VERSION,
            "error": code,
            "message": message,
        })),
    )
        .into_response()
}

/// Attach the base64 settlement header in the payer's protocol version:
/// `X-PAYMENT-RESPONSE` (v1) or `PAYMENT-RESPONSE` (v2).
pub fn attach_x402_settlement(mut resp: Response, settled: &SettleResponse, version: u32) -> Response {
    let name = if version >= X402_VERSION_V2 {
        HEADER_PAYMENT_RESPONSE_V2
    } else {
        HEADER_PAYMENT_RESPONSE
    };
    if let Ok(encoded) = turnstile_core::x402::wire::encode_header(settled) {
        if let Ok(value) = HeaderValue::from_str(&encoded) {
            resp.headers_mut().insert(HeaderName::from_static(name), value);
        }
    }
    resp.headers_mut().insert(
        HeaderName::from_static("access-control-expose-headers"),
        HeaderValue::from_static(X402_EXPOSED),
    );
    resp
}
