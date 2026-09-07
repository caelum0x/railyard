//! Payer-signed account-to-account settlement through AITP-402.
//!
//! The service API key authorizes access to Mint, but it does not stand in for
//! the account owner. The payer signs the authorization locally; Mint resolves
//! the registered `(payer, pubkey_id)` and verifies that signature before any
//! debit is committed.

use mint_protocol::{
    decode_challenge, decode_receipt, AitpReceipt, Authorization, Challenge, Scheme,
    HEADER_CHALLENGE, HEADER_RECEIPT,
};
use reqwest::Method;
use serde::{Deserialize, Serialize};

use crate::{Error, ErrorBody, ErrorEnvelope, MintClient};

#[derive(Debug, Serialize)]
pub struct AitpChallengeRequest<'a> {
    pub payee: &'a str,
    pub amount_micros: i64,
    pub resource: &'a str,
    pub scheme: Scheme,
    pub ttl_ms: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AitpSettlement {
    pub scheme: String,
    pub receipt: serde_json::Value,
    #[serde(default)]
    pub hold_id: Option<String>,
    #[serde(skip)]
    pub signed_receipt: Option<AitpReceipt>,
}

impl MintClient {
    /// Ask Mint to issue a single-use challenge for a public receive address.
    /// A valid response is deliberately HTTP 402 and must carry the same
    /// challenge in both its JSON body and authenticated protocol header.
    pub async fn issue_aitp_challenge(
        &self,
        request: &AitpChallengeRequest<'_>,
    ) -> Result<Challenge, Error> {
        let url = self
            .base
            .join("v1/aitp/challenge")
            .map_err(|error| Error::InvalidBaseUrl(error.to_string()))?;
        let response = self
            .http
            .post(url)
            .header("x-api-key", &self.api_key)
            .header("accept", "application/json")
            .json(request)
            .send()
            .await?;
        let status = response.status();
        let encoded = response
            .headers()
            .get(HEADER_CHALLENGE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let bytes = response.bytes().await?;
        if status != reqwest::StatusCode::PAYMENT_REQUIRED {
            let envelope: ErrorEnvelope =
                serde_json::from_slice(&bytes).unwrap_or_else(|_| ErrorEnvelope {
                    error: ErrorBody {
                        code: "http_error".into(),
                        message: String::from_utf8_lossy(&bytes).into_owned(),
                    },
                });
            return Err(Error::Api {
                status,
                code: envelope.error.code,
                message: envelope.error.message,
            });
        }
        let body: Challenge =
            serde_json::from_slice(&bytes).map_err(|error| Error::Protocol(error.to_string()))?;
        let header = encoded.ok_or_else(|| {
            Error::Protocol("Mint 402 response omitted the AITP-Challenge header".into())
        })?;
        let header = decode_challenge(&header)
            .map_err(|error| Error::Protocol(format!("invalid AITP challenge header: {error}")))?;
        if header != body {
            return Err(Error::Protocol(
                "AITP challenge header and JSON body disagree".into(),
            ));
        }
        Ok(body)
    }

    /// Submit the payer's locally signed authorization. Mint verifies the
    /// signature and nonce before atomically committing a transfer or hold.
    pub async fn settle_aitp(
        &self,
        authorization: &Authorization,
    ) -> Result<AitpSettlement, Error> {
        let (mut settlement, headers): (AitpSettlement, _) = self
            .send_with_headers(Method::POST, "v1/aitp/verify", Some(authorization))
            .await?;
        let encoded = headers
            .get(HEADER_RECEIPT)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| Error::Protocol("Mint settlement omitted AITP-Receipt".into()))?;
        settlement.signed_receipt = Some(
            decode_receipt(encoded)
                .map_err(|error| Error::Protocol(format!("invalid AITP receipt: {error}")))?,
        );
        Ok(settlement)
    }
}
