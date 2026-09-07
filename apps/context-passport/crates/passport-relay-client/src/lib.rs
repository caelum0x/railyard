//! Real HTTP client to the Relay gateway, implementing `RelayConditioningSink`.
//!
//! Relay (a TensorZero fork) performs the provider fan-out; the Passport supplies
//! the per-provider conditioning. This client submits a resolved
//! `RelayGenerationRequest` and returns the produced asset reference. The exact
//! Relay conditioning endpoint contract is refined in Sx1; the transport,
//! auth, and error mapping are real now and faked in tests via `FakeRelay`.

use async_trait::async_trait;
use mint_protocol::{decode_receipt, HEADER_RECEIPT};

use passport_core::{
    PassportError, PaymentEvidence, RelayConditioningSink, RelayGenerationRequest,
    RelayGenerationResult, SettlementReceiptProof,
};

const HEADER_REQUEST_ID: &str = "AITP-Request-ID";

#[derive(Clone)]
pub struct RelayClient {
    http: reqwest::Client,
    base: String,
    api_key: String,
    /// Path the conditioned-generation request is POSTed to.
    path: String,
}

impl RelayClient {
    pub fn new(base: impl Into<String>, api_key: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base: base.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            path: "/v1/passport/generate".to_string(),
        }
    }

    /// Override the Relay generation path (defaults to `/v1/passport/generate`).
    pub fn with_path(mut self, path: impl Into<String>) -> Self {
        self.path = path.into();
        self
    }
}

#[async_trait]
impl RelayConditioningSink for RelayClient {
    async fn generate(
        &self,
        request: &RelayGenerationRequest,
    ) -> Result<RelayGenerationResult, PassportError> {
        let response = self
            .http
            .post(format!("{}{}", self.base, self.path))
            .header("X-API-Key", &self.api_key)
            .json(request)
            .send()
            .await
            .map_err(|e| PassportError::Relay(e.to_string()))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(PassportError::Relay(format!(
                "relay returned {status}: {body}"
            )));
        }

        let payment_evidence = payment_evidence(response.headers(), &request.principal)?;
        let mut result = response
            .json::<RelayGenerationResult>()
            .await
            .map_err(|e| PassportError::Relay(format!("decoding relay response: {e}")))?;
        result.payment_evidence = Some(payment_evidence);
        Ok(result)
    }
}

fn payment_evidence(
    headers: &reqwest::header::HeaderMap,
    principal: &str,
) -> Result<PaymentEvidence, PassportError> {
    let encoded_receipt = headers
        .get(HEADER_RECEIPT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
        .ok_or_else(|| PassportError::Relay("Relay response missing AITP-Receipt".into()))?;
    let request_id = headers
        .get(HEADER_REQUEST_ID)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| PassportError::Relay("Relay response missing AITP-Request-ID".into()))?;
    let receipt = decode_receipt(&encoded_receipt)
        .map_err(|e| PassportError::Relay(format!("decoding Relay receipt: {e}")))?;
    if !matches!(receipt.op.as_str(), "hold.capture" | "transfer") {
        return Err(PassportError::Relay(format!(
            "Relay generation returned non-settling {} receipt",
            receipt.op
        )));
    }
    Ok(PaymentEvidence {
        request_id,
        payer_account: principal.to_string(),
        receipt: SettlementReceiptProof {
            sequence: receipt.seq,
            operation: receipt.op,
            operation_hash: receipt.op_hash,
            amount_micros: receipt.amount_micros,
            receipt_hash: receipt.hash,
            signature: receipt.sig,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::{HeaderMap, HeaderValue};

    const RECEIPT: &str = "eyJzZXEiOjQyLCJvcCI6ImhvbGQuY2FwdHVyZSIsIm9wX2hhc2giOiJhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhYWFhIiwiYW1vdW50X21pY3JvcyI6OTkwMDAwLCJiYWxhbmNlX2FmdGVyX21pY3JvcyI6NzAxMDAwMCwicHJldl9oYXNoIjoiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYmJiYiIsImhhc2giOiJjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjY2NjIiwic2lnIjoiZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWVlZWUifQ";

    #[test]
    fn decodes_principal_and_request_correlated_signed_evidence() {
        let mut headers = HeaderMap::new();
        headers.insert(HEADER_RECEIPT, HeaderValue::from_static(RECEIPT));
        headers.insert(HEADER_REQUEST_ID, HeaderValue::from_static("req_1"));
        let evidence = payment_evidence(&headers, "acct_owner").unwrap();
        assert_eq!(evidence.request_id, "req_1");
        assert_eq!(evidence.payer_account, "acct_owner");
        assert_eq!(evidence.receipt.operation, "hold.capture");
        assert_eq!(evidence.receipt.sequence, 42);
        assert!(!evidence.receipt.signature.is_empty());
    }

    #[test]
    fn missing_receipt_fails_closed() {
        let mut headers = HeaderMap::new();
        headers.insert(HEADER_REQUEST_ID, HeaderValue::from_static("req_1"));
        let error = payment_evidence(&headers, "acct_owner").unwrap_err();
        assert!(matches!(error, PassportError::Relay(message) if message.contains("AITP-Receipt")));
    }
}
