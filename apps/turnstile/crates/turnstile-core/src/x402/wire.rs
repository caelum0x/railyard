//! x402 wire types (protocol version 1) and the base64 header codec.
//!
//! These mirror the open x402 specification (x402 Foundation / Coinbase,
//! `specs/x402-specification.md`):
//!
//! - a `402` response carries a JSON [`PaymentRequiredBody`] whose `accepts`
//!   array lists every [`PaymentRequirements`] the server will take;
//! - the client retries with an `X-PAYMENT` header holding a base64-encoded
//!   JSON [`PaymentPayload`] (for the `exact` EVM scheme: an EIP-3009
//!   `transferWithAuthorization` signature);
//! - the server verifies and settles through a facilitator (`POST /verify`,
//!   `POST /settle`) and returns the base64 JSON [`SettleResponse`] in
//!   `X-PAYMENT-RESPONSE`.
//!
//! Field names are camelCase on the wire exactly as the spec defines them.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The x402 protocol version this module speaks.
pub const X402_VERSION: u32 = 1;

/// The only scheme Turnstile advertises: a fixed-amount EIP-3009 transfer.
pub const SCHEME_EXACT: &str = "exact";

/// Request header carrying the client's signed payment (x402 v1).
pub const HEADER_PAYMENT: &str = "x-payment";
/// Response header carrying the facilitator settlement result.
pub const HEADER_PAYMENT_RESPONSE: &str = "x-payment-response";

/// One way the server is willing to be paid for a resource.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequirements {
    /// Payment scheme, always `exact` here.
    pub scheme: String,
    /// Chain identifier understood by the facilitator (e.g. `base`, `base-sepolia`).
    pub network: String,
    /// Price in the asset's atomic units, as a decimal string.
    pub max_amount_required: String,
    /// The URL (or path) of the paid resource.
    pub resource: String,
    /// Human-readable description of what is being paid for.
    pub description: String,
    /// MIME type of the paid response.
    pub mime_type: String,
    /// Address that receives the funds.
    pub pay_to: String,
    /// Upper bound on how long the server may take to respond.
    pub max_timeout_seconds: u64,
    /// ERC-20 token contract of the asset (USDC).
    pub asset: String,
    /// Optional JSON schema of the response body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_schema: Option<Value>,
    /// Scheme-specific extras; for `exact` EVM the EIP-712 domain `name`/`version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

/// The JSON body of an x402 `402 Payment Required` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequiredBody {
    pub x402_version: u32,
    pub error: String,
    pub accepts: Vec<PaymentRequirements>,
}

/// The decoded `X-PAYMENT` header.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentPayload {
    pub x402_version: u32,
    pub scheme: String,
    pub network: String,
    /// Scheme-specific payload; for `exact` EVM `{ signature, authorization }`.
    pub payload: Value,
}

/// The EIP-3009 authorization inside an `exact` EVM payload.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExactAuthorization {
    pub from: String,
    pub to: String,
    pub value: String,
    pub valid_after: String,
    pub valid_before: String,
    pub nonce: String,
}

impl PaymentPayload {
    /// Extract the EIP-3009 authorization of an `exact` EVM payload.
    pub fn exact_authorization(&self) -> Result<ExactAuthorization, String> {
        let authorization = self
            .payload
            .get("authorization")
            .ok_or("payload.authorization is missing")?;
        serde_json::from_value(authorization.clone())
            .map_err(|e| format!("payload.authorization is malformed: {e}"))
    }
}

/// Facilitator `POST /verify` and `POST /settle` request body.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FacilitatorRequest<'a> {
    pub x402_version: u32,
    pub payment_payload: &'a PaymentPayload,
    pub payment_requirements: &'a PaymentRequirements,
}

/// Facilitator `POST /verify` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifyResponse {
    pub is_valid: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invalid_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
}

/// Facilitator `POST /settle` response, echoed to the client in
/// `X-PAYMENT-RESPONSE`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettleResponse {
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_reason: Option<String>,
    /// On-chain transaction hash (empty on failure).
    #[serde(default)]
    pub transaction: String,
    #[serde(default)]
    pub network: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payer: Option<String>,
}

/// Decode a base64 JSON `X-PAYMENT` header.
pub fn decode_payment_header(raw: &str) -> Result<PaymentPayload, String> {
    let bytes = STANDARD
        .decode(raw.trim())
        .map_err(|e| format!("X-PAYMENT is not valid base64: {e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("X-PAYMENT is not a valid payload: {e}"))
}

/// Encode any serializable value as base64 JSON (payment headers).
pub fn encode_header<T: Serialize>(value: &T) -> Result<String, String> {
    let json = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    Ok(STANDARD.encode(json))
}

/// `true` when two EVM addresses are equal ignoring checksum case.
#[must_use]
pub fn same_address(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn payment_header_round_trips_through_base64_json() {
        let payload = PaymentPayload {
            x402_version: 1,
            scheme: "exact".into(),
            network: "base-sepolia".into(),
            payload: json!({
                "signature": "0xsig",
                "authorization": {
                    "from": "0xaaa", "to": "0xbbb", "value": "1000",
                    "validAfter": "0", "validBefore": "9999999999", "nonce": "0x01"
                }
            }),
        };
        let header = encode_header(&payload).unwrap();
        let decoded = decode_payment_header(&header).unwrap();
        assert_eq!(decoded, payload);
        let auth = decoded.exact_authorization().unwrap();
        assert_eq!(auth.value, "1000");
        assert_eq!(auth.to, "0xbbb");
    }

    #[test]
    fn requirements_serialize_with_spec_field_names() {
        let req = PaymentRequirements {
            scheme: "exact".into(),
            network: "base".into(),
            max_amount_required: "10000".into(),
            resource: "https://api.example.com/v1/tools/x/call".into(),
            description: "x".into(),
            mime_type: "application/json".into(),
            pay_to: "0xabc".into(),
            max_timeout_seconds: 60,
            asset: "0xusdc".into(),
            output_schema: None,
            extra: Some(json!({"name": "USD Coin", "version": "2"})),
        };
        let v = serde_json::to_value(&req).unwrap();
        assert_eq!(v["maxAmountRequired"], "10000");
        assert_eq!(v["payTo"], "0xabc");
        assert_eq!(v["maxTimeoutSeconds"], 60);
        assert!(v.get("outputSchema").is_none());
    }

    #[test]
    fn garbage_header_is_rejected() {
        assert!(decode_payment_header("%%%").is_err());
        assert!(decode_payment_header(&STANDARD.encode(b"{}")).is_err());
    }
}
