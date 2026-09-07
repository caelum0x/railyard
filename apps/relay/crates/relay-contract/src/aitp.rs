//! Public AITP-402 challenge and payment-required projection.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use ts_rs::TS;
use uuid::Uuid;

use crate::money::Microdollars;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
pub enum AitpScheme {
    #[serde(rename = "aitp-hold")]
    #[ts(rename = "aitp-hold")]
    Hold,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct AitpChallenge {
    pub payee: String,
    pub amount_micros: Microdollars,
    pub currency: String,
    pub resource: String,
    pub nonce: String,
    pub expiry: u64,
    pub scheme: AitpScheme,
}

impl AitpChallenge {
    pub fn decode(encoded: &str) -> Result<Self, AitpChallengeDecodeError> {
        let bytes = URL_SAFE_NO_PAD
            .decode(encoded.as_bytes())
            .map_err(|_| AitpChallengeDecodeError::InvalidBase64)?;
        let challenge: Self =
            serde_json::from_slice(&bytes).map_err(|_| AitpChallengeDecodeError::InvalidJson)?;
        challenge.validate()?;
        Ok(challenge)
    }

    pub fn encode(&self) -> Result<String, AitpChallengeDecodeError> {
        self.validate()?;
        serde_json::to_vec(self)
            .map(|bytes| URL_SAFE_NO_PAD.encode(bytes))
            .map_err(|_| AitpChallengeDecodeError::InvalidJson)
    }

    pub fn validate(&self) -> Result<(), AitpChallengeDecodeError> {
        if self.payee.is_empty()
            || self.resource.is_empty()
            || self.nonce.is_empty()
            || self.currency != "AIT"
            || self.amount_micros.get() < 0
        {
            return Err(AitpChallengeDecodeError::InvalidFields);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
pub struct PaymentRequired {
    pub challenge: AitpChallenge,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub request_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub protocol_version: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum AitpChallengeDecodeError {
    #[error("AITP challenge is not valid base64url")]
    InvalidBase64,
    #[error("AITP challenge is not valid JSON")]
    InvalidJson,
    #[error("AITP challenge contains invalid protocol fields")]
    InvalidFields,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_round_trips_without_padding() {
        let challenge = AitpChallenge {
            payee: "ait1qrelay".into(),
            amount_micros: Microdollars::new(42),
            currency: "AIT".into(),
            resource: "POST /openai/v1/chat/completions#m@id".into(),
            nonce: "nonce".into(),
            expiry: 1_800_000_000_000,
            scheme: AitpScheme::Hold,
        };
        let encoded = challenge.encode().unwrap();
        assert!(!encoded.contains('='));
        assert_eq!(AitpChallenge::decode(&encoded), Ok(challenge));
    }
}
