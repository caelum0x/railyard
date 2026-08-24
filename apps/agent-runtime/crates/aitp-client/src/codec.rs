//! Base64url header codec for the AITP-402 handshake (SPEC §5).
//!
//! Challenge/authorization/receipt travel as `base64url(deterministic JSON)` in
//! HTTP headers. These helpers are deliberately separate from the gateway trait
//! so the wire encoding can be tested without a signer or a network.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::{de::DeserializeOwned, Serialize};

use crate::error::PayError;
use crate::types::{PaymentAuthorization, PaymentChallenge, Receipt};

/// AITP protocol version this client speaks (`AITP-Version` header).
pub const AITP_VERSION: &str = "0.1";

/// Header names from SPEC §5.
pub const HEADER_VERSION: &str = "AITP-Version";
pub const HEADER_CHALLENGE: &str = "AITP-Challenge";
pub const HEADER_AUTHORIZATION: &str = "AITP-Authorization";
pub const HEADER_RECEIPT: &str = "AITP-Receipt";

fn encode<T: Serialize>(value: &T) -> Result<String, PayError> {
    let json = serde_json::to_vec(value).map_err(|e| PayError::Codec(e.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(json))
}

fn decode<T: DeserializeOwned>(header: &str) -> Result<T, PayError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(header.trim())
        .map_err(|e| PayError::Codec(e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| PayError::Codec(e.to_string()))
}

/// Decode an incoming `AITP-Challenge` header.
pub fn decode_challenge(header: &str) -> Result<PaymentChallenge, PayError> {
    decode(header)
}

/// Encode a `PaymentChallenge` (used by fakes/tests to simulate a 402).
pub fn encode_challenge(challenge: &PaymentChallenge) -> Result<String, PayError> {
    encode(challenge)
}

/// Encode an outgoing `AITP-Authorization` header.
pub fn encode_authorization(auth: &PaymentAuthorization) -> Result<String, PayError> {
    encode(auth)
}

/// Decode an `AITP-Authorization` header (server-side/testing).
pub fn decode_authorization(header: &str) -> Result<PaymentAuthorization, PayError> {
    decode(header)
}

/// Decode an incoming `AITP-Receipt` header.
pub fn decode_receipt(header: &str) -> Result<Receipt, PayError> {
    decode(header)
}

/// Encode a `Receipt` (used by fakes/tests).
pub fn encode_receipt(receipt: &Receipt) -> Result<String, PayError> {
    encode(receipt)
}
