//! Base64url JSON header codec for AITP-402.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{PaymentAuthorization, PaymentChallenge, Receipt, WireError};

pub const AITP_VERSION: &str = "0.1";
pub const HEADER_VERSION: &str = "AITP-Version";
pub const HEADER_CHALLENGE: &str = "AITP-Challenge";
pub const HEADER_AUTHORIZATION: &str = "AITP-Authorization";
pub const HEADER_RECEIPT: &str = "AITP-Receipt";
pub const HEADER_REQUEST_ID: &str = "AITP-Request-ID";

fn encode<T: Serialize>(value: &T) -> Result<String, WireError> {
    let json = serde_json::to_vec(value).map_err(|error| WireError::Codec(error.to_string()))?;
    Ok(URL_SAFE_NO_PAD.encode(json))
}

fn decode<T: DeserializeOwned>(header: &str) -> Result<T, WireError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(header.trim())
        .map_err(|error| WireError::Codec(error.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|error| WireError::Codec(error.to_string()))
}

pub fn decode_challenge(header: &str) -> Result<PaymentChallenge, WireError> {
    decode(header)
}

pub fn encode_challenge(challenge: &PaymentChallenge) -> Result<String, WireError> {
    encode(challenge)
}

pub fn encode_authorization(authorization: &PaymentAuthorization) -> Result<String, WireError> {
    encode(authorization)
}

pub fn decode_authorization(header: &str) -> Result<PaymentAuthorization, WireError> {
    decode(header)
}

pub fn decode_receipt(header: &str) -> Result<Receipt, WireError> {
    decode(header)
}

pub fn encode_receipt(receipt: &Receipt) -> Result<String, WireError> {
    encode(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AmountMicros, Scheme};

    #[test]
    fn authorization_round_trips_without_padding() {
        let authorization = PaymentAuthorization {
            payer: "acct_payer".into(),
            payee: "ait1qrelay".into(),
            amount_micros: AmountMicros::new(42),
            nonce: "nonce".into(),
            resource: "POST /inference#model".into(),
            expiry: 1_800_000_000_000,
            scheme: Scheme::Hold,
            idempotency_key: "idem".into(),
            pubkey_id: "key".into(),
            op_hash: "00".repeat(32),
            sig: "signature".into(),
        };
        let encoded = encode_authorization(&authorization).expect("encode");
        assert!(!encoded.contains('='));
        assert_eq!(
            decode_authorization(&encoded).expect("decode"),
            authorization
        );
    }
}
