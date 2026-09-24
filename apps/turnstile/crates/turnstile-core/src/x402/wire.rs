//! x402 wire types (protocol versions 1 and 2) and the base64 header codec.
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
//!
//! x402 **v2** (`specs/x402-specification-v2.md`, `transports-v2/http.md`)
//! moves the protocol into headers and CAIP-2 network ids:
//!
//! - `PAYMENT-REQUIRED` (base64 [`PaymentRequiredV2`]) on the `402`;
//! - `PAYMENT-SIGNATURE` (base64 v2 [`PaymentPayload`], whose `accepted` echoes
//!   the chosen [`PaymentRequirementsV2`]) on the retry;
//! - `PAYMENT-RESPONSE` (base64 [`SettleResponse`]) on the paid response.
//!
//! Turnstile speaks both at once: the `402` carries the v1 JSON body AND the v2
//! header, and a retry may use either `X-PAYMENT` (v1) or `PAYMENT-SIGNATURE`
//! (v2). [`PaymentPayload`] deserializes either version.

use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The x402 v1 protocol version (the JSON `402` body, `X-PAYMENT`).
pub const X402_VERSION: u32 = 1;
/// The x402 v2 protocol version (`PAYMENT-*` headers, CAIP-2 networks).
pub const X402_VERSION_V2: u32 = 2;

/// The only scheme Turnstile advertises: a fixed-amount EIP-3009 transfer.
pub const SCHEME_EXACT: &str = "exact";

/// Request header carrying the client's signed payment (x402 v1).
pub const HEADER_PAYMENT: &str = "x-payment";
/// Response header carrying the facilitator settlement result (x402 v1).
pub const HEADER_PAYMENT_RESPONSE: &str = "x-payment-response";
/// Request header carrying the client's signed payment (x402 v2).
pub const HEADER_PAYMENT_SIGNATURE: &str = "payment-signature";
/// `402` header carrying the base64 [`PaymentRequiredV2`] (x402 v2).
pub const HEADER_PAYMENT_REQUIRED: &str = "payment-required";
/// Response header carrying the settlement result (x402 v2).
pub const HEADER_PAYMENT_RESPONSE_V2: &str = "payment-response";

/// v1 network names ↔ CAIP-2 ids for the EVM chains facilitators settle on.
const NETWORKS: &[(&str, &str)] = &[
    ("base", "eip155:8453"),
    ("base-sepolia", "eip155:84532"),
    ("avalanche", "eip155:43114"),
    ("avalanche-fuji", "eip155:43113"),
    ("polygon", "eip155:137"),
    ("polygon-amoy", "eip155:80002"),
    ("sei", "eip155:1329"),
    ("sei-testnet", "eip155:1328"),
    ("iotex", "eip155:4689"),
];

/// The CAIP-2 id (v2) for a network given in either form.
#[must_use]
pub fn caip2_network(network: &str) -> String {
    NETWORKS
        .iter()
        .find(|(v1, _)| *v1 == network)
        .map_or_else(|| network.to_string(), |(_, caip2)| (*caip2).to_string())
}

/// The v1 network name for a network given in either form (a CAIP-2 id with
/// no v1 name is returned unchanged).
#[must_use]
pub fn v1_network(network: &str) -> String {
    NETWORKS
        .iter()
        .find(|(_, caip2)| *caip2 == network)
        .map_or_else(|| network.to_string(), |(v1, _)| (*v1).to_string())
}

/// `true` when two network ids name the same chain (either form).
#[must_use]
pub fn same_network(a: &str, b: &str) -> bool {
    caip2_network(a.trim()) == caip2_network(b.trim())
}

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

impl PaymentRequirements {
    /// The same requirement in x402 v2 form (CAIP-2 network, `amount`).
    #[must_use]
    pub fn to_v2(&self) -> PaymentRequirementsV2 {
        PaymentRequirementsV2 {
            scheme: self.scheme.clone(),
            network: caip2_network(&self.network),
            amount: self.max_amount_required.clone(),
            asset: self.asset.clone(),
            pay_to: self.pay_to.clone(),
            max_timeout_seconds: self.max_timeout_seconds,
            extra: self.extra.clone(),
        }
    }

    /// The v2 `ResourceInfo` describing this requirement's resource.
    #[must_use]
    pub fn resource_info(&self) -> ResourceInfo {
        ResourceInfo {
            url: self.resource.clone(),
            description: Some(self.description.clone()).filter(|d| !d.is_empty()),
            mime_type: Some(self.mime_type.clone()).filter(|m| !m.is_empty()),
        }
    }
}

/// x402 v2 `PaymentRequirements`: resource metadata moved to [`ResourceInfo`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequirementsV2 {
    pub scheme: String,
    /// CAIP-2 network id (e.g. `eip155:8453`).
    pub network: String,
    /// Price in atomic units, as a decimal string.
    pub amount: String,
    pub asset: String,
    pub pay_to: String,
    pub max_timeout_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Value>,
}

/// x402 v2 `ResourceInfo`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceInfo {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

/// x402 v2 `PaymentRequired` (the `PAYMENT-REQUIRED` header, and the MCP
/// `structuredContent` of an unpaid tool call).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentRequiredV2 {
    pub x402_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub resource: ResourceInfo,
    pub accepts: Vec<PaymentRequirementsV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Value>,
}

impl PaymentRequiredV2 {
    /// The v2 form of a v1 `402` body (`None` when it accepts nothing).
    #[must_use]
    pub fn from_v1(body: &PaymentRequiredBody) -> Option<Self> {
        let first = body.accepts.first()?;
        Some(Self {
            x402_version: X402_VERSION_V2,
            error: Some(body.error.clone()).filter(|e| !e.is_empty()),
            resource: first.resource_info(),
            accepts: body.accepts.iter().map(PaymentRequirements::to_v2).collect(),
            extensions: None,
        })
    }
}

/// A decoded payment: `X-PAYMENT` (v1) or `PAYMENT-SIGNATURE` / MCP
/// `_meta["x402/payment"]` (v2). v1 names `scheme`/`network` at the top level;
/// v2 echoes the chosen requirement in `accepted`. Use [`scheme`](Self::scheme)
/// and [`network`](Self::network) to read either.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PaymentPayload {
    pub x402_version: u32,
    /// v1 only.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub scheme: String,
    /// v1 only.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub network: String,
    /// v2: the resource the client paid for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<ResourceInfo>,
    /// v2: the requirement the client chose from `accepts`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted: Option<PaymentRequirementsV2>,
    /// Scheme-specific payload; for `exact` EVM `{ signature, authorization }`.
    pub payload: Value,
    /// v2 extension data echoed by the client.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Value>,
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
    /// A v1 payload.
    #[must_use]
    pub fn v1(scheme: &str, network: &str, payload: Value) -> Self {
        Self {
            x402_version: X402_VERSION,
            scheme: scheme.to_string(),
            network: network.to_string(),
            payload,
            ..Self::default()
        }
    }

    /// The payment scheme, whichever version carried it.
    #[must_use]
    pub fn scheme(&self) -> &str {
        match &self.accepted {
            Some(accepted) if self.x402_version >= X402_VERSION_V2 => &accepted.scheme,
            _ => &self.scheme,
        }
    }

    /// The network, whichever version carried it (as the client sent it).
    #[must_use]
    pub fn network(&self) -> &str {
        match &self.accepted {
            Some(accepted) if self.x402_version >= X402_VERSION_V2 => &accepted.network,
            _ => &self.network,
        }
    }

    /// Structural validation per version: v1 needs `scheme` + `network`, v2
    /// needs `accepted`; any other version is refused.
    pub fn check_shape(&self) -> Result<(), String> {
        match self.x402_version {
            X402_VERSION if self.scheme.is_empty() || self.network.is_empty() => {
                Err("x402 v1 payment needs scheme and network".into())
            }
            X402_VERSION_V2 if self.accepted.is_none() => {
                Err("x402 v2 payment needs `accepted` (the chosen requirement)".into())
            }
            X402_VERSION | X402_VERSION_V2 => Ok(()),
            other => Err(format!("x402Version {other} is not supported (expected 1 or 2)")),
        }
    }

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

/// Facilitator `POST /verify` and `POST /settle` request body. `R` is
/// [`PaymentRequirements`] for v1 and [`PaymentRequirementsV2`] for v2.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FacilitatorRequest<'a, R: Serialize> {
    pub x402_version: u32,
    pub payment_payload: &'a PaymentPayload,
    pub payment_requirements: &'a R,
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

/// Decode a base64 JSON `X-PAYMENT` / `PAYMENT-SIGNATURE` header (either
/// version) and check its shape.
pub fn decode_payment_header(raw: &str) -> Result<PaymentPayload, String> {
    let bytes = STANDARD
        .decode(raw.trim())
        .map_err(|e| format!("payment header is not valid base64: {e}"))?;
    let payload: PaymentPayload = serde_json::from_slice(&bytes)
        .map_err(|e| format!("payment header is not a valid x402 payload: {e}"))?;
    payload.check_shape()?;
    Ok(payload)
}

/// Decode a JSON payment (MCP `_meta["x402/payment"]`): an object, or a base64
/// header string for clients that forward the HTTP header value verbatim.
pub fn payment_from_json(value: &Value) -> Result<PaymentPayload, String> {
    match value {
        Value::String(raw) => decode_payment_header(raw),
        Value::Object(_) => {
            let payload: PaymentPayload = serde_json::from_value(value.clone())
                .map_err(|e| format!("x402/payment is not a valid x402 payload: {e}"))?;
            payload.check_shape()?;
            Ok(payload)
        }
        _ => Err("x402/payment must be a PaymentPayload object".into()),
    }
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
        let payload = PaymentPayload::v1(
            "exact",
            "base-sepolia",
            json!({
                "signature": "0xsig",
                "authorization": {
                    "from": "0xaaa", "to": "0xbbb", "value": "1000",
                    "validAfter": "0", "validBefore": "9999999999", "nonce": "0x01"
                }
            }),
        );
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

    /// The spec's `PAYMENT-SIGNATURE` example (transports-v2/http.md), verbatim.
    const SPEC_V2_HEADER: &str = "eyJ4NDAyVmVyc2lvbiI6MiwicmVzb3VyY2UiOnsidXJsIjoiaHR0cHM6Ly9hcGkuZXhhbXBsZS5jb20vcHJlbWl1bS1kYXRhIiwiZGVzY3JpcHRpb24iOiJBY2Nlc3MgdG8gcHJlbWl1bSBtYXJrZXQgZGF0YSIsIm1pbWVUeXBlIjoiYXBwbGljYXRpb24vanNvbiJ9LCJhY2NlcHRlZCI6eyJzY2hlbWUiOiJleGFjdCIsIm5ldHdvcmsiOiJlaXAxNTU6ODQ1MzIiLCJhbW91bnQiOiIxMDAwMCIsImFzc2V0IjoiMHgwMzZDYkQ1Mzg0MmM1NDI2NjM0ZTc5Mjk1NDFlQzIzMThmM2RDRjdlIiwicGF5VG8iOiIweDIwOTY5M0JjNmFmYzBDNTMyOGJBMzZGYUYwM0M1MTRFRjMxMjI4N0MiLCJtYXhUaW1lb3V0U2Vjb25kcyI6NjAsImV4dHJhIjp7Im5hbWUiOiJVU0RDIiwidmVyc2lvbiI6IjIifX0sInBheWxvYWQiOnsic2lnbmF0dXJlIjoiMHgyZDZhNzU4OGQ2YWNjYTUwNWNiZjBkOWE0YTIyN2UwYzUyYzZjMzQwMDhjOGU4OTg2YTEyODMyNTk3NjQxNzM2MDhhMmNlNjQ5NjY0MmUzNzdkNmRhOGRiYmY1ODM2ZTliZDE1MDkyZjllY2FiMDVkZWQzZDYyOTNhZjE0OGI1NzFjIiwiYXV0aG9yaXphdGlvbiI6eyJmcm9tIjoiMHg4NTdiMDY1MTlFOTFlM0E1NDUzODc5MWJEYmIwRTIyMzczZTM2YjY2IiwidG8iOiIweDIwOTY5M0JjNmFmYzBDNTMyOGJBMzZGYUYwM0M1MTRFRjMxMjI4N0MiLCJ2YWx1ZSI6IjEwMDAwIiwidmFsaWRBZnRlciI6IjE3NDA2NzIwODkiLCJ2YWxpZEJlZm9yZSI6IjE3NDA2NzIxNTQiLCJub25jZSI6IjB4ZjM3NDY2MTNjMmQ5MjBiNWZkYWJjMDg1NmYyYWViMmQ0Zjg4ZWU2MDM3YjhjYzVkMDRhNzFhNDQ2MmYxMzQ4MCJ9fX0=";

    #[test]
    fn spec_v2_payment_signature_header_parses() {
        let payload = decode_payment_header(SPEC_V2_HEADER).unwrap();
        assert_eq!(payload.x402_version, 2);
        assert_eq!(payload.scheme(), "exact");
        assert_eq!(payload.network(), "eip155:84532");
        let accepted = payload.accepted.as_ref().unwrap();
        assert_eq!(accepted.amount, "10000");
        assert_eq!(accepted.pay_to, "0x209693Bc6afc0C5328bA36FaF03C514EF312287C");
        assert_eq!(
            payload.resource.as_ref().unwrap().url,
            "https://api.example.com/premium-data"
        );
        let auth = payload.exact_authorization().unwrap();
        assert_eq!(auth.value, "10000");
        assert_eq!(auth.valid_before, "1740672154");
        // Re-encoding keeps the v2 shape (no empty v1 fields leak in).
        let v = serde_json::to_value(&payload).unwrap();
        assert!(v.get("scheme").is_none());
        assert_eq!(v["accepted"]["network"], "eip155:84532");
    }

    #[test]
    fn version_and_shape_are_enforced() {
        let v2_without_accepted = json!({ "x402Version": 2, "payload": {} });
        assert!(decode_payment_header(&STANDARD.encode(v2_without_accepted.to_string()))
            .unwrap_err()
            .contains("accepted"));
        let v3 = json!({ "x402Version": 3, "scheme": "exact", "network": "base", "payload": {} });
        assert!(decode_payment_header(&STANDARD.encode(v3.to_string()))
            .unwrap_err()
            .contains("not supported"));
        let v1_missing_network = json!({ "x402Version": 1, "scheme": "exact", "payload": {} });
        assert!(decode_payment_header(&STANDARD.encode(v1_missing_network.to_string())).is_err());
    }

    #[test]
    fn mcp_meta_payment_accepts_object_or_base64_string() {
        let from_header = decode_payment_header(SPEC_V2_HEADER).unwrap();
        let object = serde_json::to_value(&from_header).unwrap();
        assert_eq!(payment_from_json(&object).unwrap(), from_header);
        assert_eq!(
            payment_from_json(&Value::String(SPEC_V2_HEADER.into())).unwrap(),
            from_header
        );
        assert!(payment_from_json(&json!(42)).is_err());
    }

    #[test]
    fn networks_map_between_v1_names_and_caip2() {
        assert_eq!(caip2_network("base"), "eip155:8453");
        assert_eq!(caip2_network("eip155:84532"), "eip155:84532");
        assert_eq!(v1_network("eip155:84532"), "base-sepolia");
        assert_eq!(v1_network("eip155:999999"), "eip155:999999");
        assert!(same_network("base-sepolia", "eip155:84532"));
        assert!(!same_network("base", "eip155:84532"));
    }

    #[test]
    fn v1_body_converts_to_v2_payment_required() {
        let req = PaymentRequirements {
            scheme: "exact".into(),
            network: "base-sepolia".into(),
            max_amount_required: "5000".into(),
            resource: "https://t.example/v1/echo".into(),
            description: "Echo".into(),
            mime_type: "application/json".into(),
            pay_to: "0xabc".into(),
            max_timeout_seconds: 60,
            asset: "0xusdc".into(),
            output_schema: None,
            extra: Some(json!({"name": "USD Coin", "version": "2"})),
        };
        let body = PaymentRequiredBody {
            x402_version: 1,
            error: "payment required".into(),
            accepts: vec![req],
        };
        let v2 = PaymentRequiredV2::from_v1(&body).unwrap();
        let v = serde_json::to_value(&v2).unwrap();
        assert_eq!(v["x402Version"], 2);
        assert_eq!(v["resource"]["url"], "https://t.example/v1/echo");
        assert_eq!(v["resource"]["mimeType"], "application/json");
        assert_eq!(v["accepts"][0]["network"], "eip155:84532");
        assert_eq!(v["accepts"][0]["amount"], "5000");
        assert!(v["accepts"][0].get("maxAmountRequired").is_none());
        assert!(PaymentRequiredV2::from_v1(&PaymentRequiredBody {
            x402_version: 1,
            error: String::new(),
            accepts: vec![],
        })
        .is_none());
    }
}
