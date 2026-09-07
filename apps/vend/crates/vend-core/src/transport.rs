//! Injectable transport TRAITS + their wire DTOs.
//!
//! [`MintTransport`] and [`RelayTransport`] are the seams that keep every other
//! domain crate free of any HTTP dependency: the real clients live in
//! `vend-mint-client` / `vend-relay-client`, the in-memory fakes in
//! `vend-testkit`. All money amounts on the wire are bare `i64` µ$.

use async_trait::async_trait;
use futures::Stream;
use serde::{Deserialize, Serialize};
use std::pin::Pin;

use crate::error::VendError;

// ---------------------------------------------------------------------------
// Mint transport (accounts / holds / capture / release / reserve)
// ---------------------------------------------------------------------------

/// Request to open a reserve hold on Mint. `idem_key` is deterministic per job
/// and reused verbatim across the hold/capture/release triple.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HoldRequest {
    /// Mint account the hold is placed against.
    pub from: String,
    /// Estimated maximum charge to reserve, in µ$.
    pub amount_micros: i64,
    /// Operation-specific idempotency key (`{request}:hold`).
    pub idem_key: String,
}

/// A successfully opened hold. Mint's create receipt does not echo the held
/// amount; Vend keeps the requested amount in its durable usage intent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HoldResponse {
    /// Mint hold id (`hold_<ULID>`).
    pub hold_id: String,
}

/// Mint's authoritative lifecycle for a hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HoldStatus {
    Open,
    Captured,
    Released,
}

/// Exact `GET /v1/holds/:id` data projection returned by mint-server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HoldView {
    pub id: String,
    pub account_id: String,
    pub amount_micros: i64,
    pub captured_micros: i64,
    pub status: HoldStatus,
    #[serde(rename = "ref")]
    pub ref_: Option<String>,
}

/// Capture the EXACT actual cost against an existing hold (never the estimate).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureRequest {
    /// Exact actual amount to capture, in µ$ (`<=` the hold).
    pub amount_micros: i64,
    /// Mint account credited by capture.
    pub to: String,
    /// Capture-specific idempotency key (`{request}:capture`).
    pub idem_key: String,
}

/// Result of a capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureResponse {
    /// Amount actually captured, in µ$.
    pub captured_micros: i64,
    #[serde(default)]
    pub proof: SettlementProof,
}

/// Signed, hash-linked evidence returned by Mint for a terminal hold operation.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementProof {
    pub sequence: u64,
    pub operation: String,
    pub operation_hash: String,
    pub receipt_hash: String,
    pub signature: String,
}

/// Release an unused hold (any non-2xx / aborted stream / panic path).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseRequest {
    /// Release-specific idempotency key (`{request}:release`).
    pub idem_key: String,
}

/// Result of a release.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseResponse {
    #[serde(default)]
    pub proof: SettlementProof,
}

/// Exact `POST /v1/transfers` request accepted by mint-server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferRequest {
    pub from: String,
    pub to: String,
    pub amount_micros: i64,
    pub idem_key: String,
    #[serde(default, rename = "ref")]
    pub ref_: Option<String>,
}

/// Mint balance projection embedded in a transfer receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MintBalance {
    pub account_id: String,
    pub available_micros: i64,
    pub held_micros: i64,
    pub updated_seq: u64,
}

/// Exact `data` payload returned by `POST /v1/transfers`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferResponse {
    pub first_seq: u64,
    pub from_balance: MintBalance,
    pub to_balance: MintBalance,
    pub idempotent_replay: bool,
}

/// Customer-selected hosted funding provider for a Mint account.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckoutRequest {
    pub provider: String,
    pub account_id: String,
    pub amount_micros: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckoutResponse {
    pub provider: String,
    pub checkout_id: String,
    pub checkout_url: String,
    pub deposit_ref: String,
    pub account_id: String,
    pub amount_micros: i64,
}

/// The Mint data-plane seam. Object-safe (async_trait boxes the futures) so it
/// can be stored as `Arc<dyn MintTransport>` in the server state.
#[async_trait]
pub trait MintTransport: Send + Sync {
    /// `POST /v1/checkouts` — hosted on-ramp depositing into a Mint account.
    async fn create_checkout(&self, req: CheckoutRequest) -> Result<CheckoutResponse, VendError>;
    /// `POST /v1/holds` — reserve `amount_micros` against `account_id`.
    async fn create_hold(&self, req: HoldRequest) -> Result<HoldResponse, VendError>;
    /// `GET /v1/holds/:id` — read Mint's authoritative hold projection.
    async fn get_hold(&self, hold_id: &str) -> Result<HoldView, VendError>;
    /// `POST /v1/holds/:id/capture` — capture the exact actual cost.
    async fn capture_hold(
        &self,
        hold_id: &str,
        req: CaptureRequest,
    ) -> Result<CaptureResponse, VendError>;
    /// `POST /v1/holds/:id/release` — release the unused hold.
    async fn release_hold(
        &self,
        hold_id: &str,
        req: ReleaseRequest,
    ) -> Result<ReleaseResponse, VendError>;
    /// `POST /v1/transfers` — move one payout leg from Vend clearing.
    async fn transfer(&self, req: TransferRequest) -> Result<TransferResponse, VendError>;
}

// ---------------------------------------------------------------------------
// Relay transport (OpenAI-compatible inference)
// ---------------------------------------------------------------------------

/// A request forwarded to Relay. `body` is the OpenAI-shaped payload passed
/// through verbatim; provider routing is injected per app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayRequest {
    /// Relay base path segment for the target surface, e.g. `/openai/v1`.
    pub base_path: String,
    /// Upstream bearer credential (Relay key or BYOK).
    pub bearer: String,
    /// The OpenAI-shaped request body, forwarded verbatim.
    pub body: serde_json::Value,
    /// No-store policy: when `true`, the Relay client forces
    /// `tensorzero::dryrun=true` so Relay does not persist observability data.
    /// Upstream-provider retention is configured separately by the operator.
    pub zdr: bool,
}

/// Normalized token usage. Populated from either OpenAI keys
/// (`prompt_tokens`/`completion_tokens`) or native (`input_tokens`/`output_tokens`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input / prompt tokens.
    pub input_tokens: i64,
    /// Output / completion tokens.
    pub output_tokens: i64,
    /// Relay's authoritative provider cost, converted exactly from decimal USD
    /// to integer microdollars. `None` means absent or not exactly representable.
    pub provider_cost_micros: Option<i64>,
}

/// A Relay response: upstream status, the response body, and parsed usage (if
/// the upstream reported any).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayResponse {
    /// Upstream HTTP status.
    pub status: u16,
    /// The response body (non-streaming JSON, or the assembled SSE payload).
    pub body: serde_json::Value,
    /// Parsed terminal usage, if present. `None` for no-usage responses.
    pub usage: Option<Usage>,
}

/// The byte stream returned by Relay for a streaming completion. Bytes are
/// deliberately not decoded or re-framed so Vend can preserve the exact SSE
/// representation and flush chunks as Relay produces them.
pub type RelayByteStream = Pin<Box<dyn Stream<Item = Result<Vec<u8>, VendError>> + Send + 'static>>;

/// Relay response metadata plus its live response-body stream.
pub struct RelayStreamResponse {
    /// Upstream HTTP status.
    pub status: u16,
    /// Upstream response headers. Values remain raw bytes so non-UTF8 header
    /// values are not silently changed by the transport abstraction.
    pub headers: Vec<(String, Vec<u8>)>,
    /// Upstream response bytes, in arrival order.
    pub stream: RelayByteStream,
}

impl RelayStreamResponse {
    /// `true` iff the upstream returned a 2xx status.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

impl RelayResponse {
    /// `true` iff the upstream returned a 2xx status.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The Relay inference seam. Object-safe for `Arc<dyn RelayTransport>`.
#[async_trait]
pub trait RelayTransport: Send + Sync {
    /// `POST {base_path}/chat/completions` — forward the OpenAI-shaped body.
    async fn chat_completion(&self, req: RelayRequest) -> Result<RelayResponse, VendError>;

    /// Streaming form of `POST {base_path}/chat/completions`. Implementations
    /// must resolve once response headers arrive and must not buffer the body.
    async fn chat_completion_stream(
        &self,
        _req: RelayRequest,
    ) -> Result<RelayStreamResponse, VendError> {
        Err(VendError::StreamingUnsupported)
    }
}

/// Parse a `usage` object from an OpenAI-shaped response body, accepting BOTH
/// the OpenAI keys (`prompt_tokens`/`completion_tokens`) and the native keys
/// (`input_tokens`/`output_tokens`). A `usage: null` frame yields `None`.
pub fn parse_usage(body: &serde_json::Value) -> Option<Usage> {
    let usage = body.get("usage")?;
    if usage.is_null() {
        return None;
    }
    let input = usage
        .get("prompt_tokens")
        .or_else(|| usage.get("input_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    let output = usage
        .get("completion_tokens")
        .or_else(|| usage.get("output_tokens"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    Some(Usage {
        input_tokens: input,
        output_tokens: output,
        provider_cost_micros: usage.get("tensorzero_cost").and_then(decimal_usd_to_micros),
    })
}

/// Convert a JSON decimal USD amount to integer microdollars without using
/// binary floating point. More than six fractional decimal places is rejected.
fn decimal_usd_to_micros(value: &serde_json::Value) -> Option<i64> {
    let raw = value.as_number()?.to_string();
    if raw.starts_with('-') {
        return None;
    }
    let (mantissa, exponent) = match raw.split_once(['e', 'E']) {
        Some((mantissa, exponent)) => (mantissa, exponent.parse::<i32>().ok()?),
        None => (raw.as_str(), 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    if whole.is_empty()
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let mut digits = format!("{whole}{fraction}");
    let scale = i32::try_from(fraction.len()).ok()?.checked_sub(exponent)?;
    if scale > 6 {
        return None;
    }
    let zeroes = usize::try_from(6_i32.checked_sub(scale)?).ok()?;
    digits.extend(std::iter::repeat_n('0', zeroes));
    let micros = digits.parse::<i128>().ok()?;
    i64::try_from(micros).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_openai_usage_keys() {
        let body = json!({"usage": {"prompt_tokens": 10, "completion_tokens": 5}});
        assert_eq!(
            parse_usage(&body),
            Some(Usage {
                input_tokens: 10,
                output_tokens: 5,
                provider_cost_micros: None,
            })
        );
    }

    #[test]
    fn parses_native_usage_keys() {
        let body = json!({"usage": {"input_tokens": 3, "output_tokens": 7}});
        assert_eq!(
            parse_usage(&body),
            Some(Usage {
                input_tokens: 3,
                output_tokens: 7,
                provider_cost_micros: None,
            })
        );
    }

    #[test]
    fn null_usage_is_none() {
        assert_eq!(parse_usage(&json!({"usage": null})), None);
        assert_eq!(parse_usage(&json!({})), None);
    }

    #[test]
    fn parses_tensorzero_cost_exactly_and_rejects_sub_micro_precision() {
        assert_eq!(
            parse_usage(&json!({
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1,
                    "tensorzero_cost": 0.123456
                }
            }))
            .unwrap()
            .provider_cost_micros,
            Some(123_456)
        );
        assert_eq!(
            parse_usage(&json!({
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1,
                    "tensorzero_cost": 0.0000001
                }
            }))
            .unwrap()
            .provider_cost_micros,
            None
        );
    }
}
