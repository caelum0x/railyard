//! Injectable transport TRAITS + their wire DTOs.
//!
//! [`MintTransport`] and [`UpstreamTransport`] are the seams that keep the proxy
//! free of any HTTP dependency: the real clients live in `abacus-mint-client` /
//! `abacus-upstream`, the in-memory fakes in `abacus-testkit`. The trait lives
//! HERE so the proxy depends on the abstraction, not the wire. All money amounts
//! on the wire are bare `i64` µ$. Port of vend-core::transport.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::AbacusError;

// ---------------------------------------------------------------------------
// Mint transport — direct holds path AND allowance-ceiling path
// ---------------------------------------------------------------------------

/// Open a reserve hold on Mint. `idem_key` is deterministic per request and
/// reused verbatim across the hold/capture/release triple (with distinct
/// `:hold`/`:capture`/`:release` suffixes applied by the client).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hold {
    /// Mint account the hold is placed against (funding/treasury `acct_…`).
    pub account_id: String,
    /// Estimated MAX charge to reserve, in µ$ (`est_max`).
    pub amount_micros: i64,
    /// Deterministic idempotency key for this request.
    pub idem_key: String,
}

/// A successfully opened hold.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HoldReceipt {
    /// Mint hold id (`hold_…`).
    pub hold_id: String,
    /// The reserved amount, in µ$.
    pub amount_micros: i64,
}

/// Capture the EXACT actual cost against an existing hold (never the estimate).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capture {
    /// Exact actual amount to capture, in µ$ (`<=` the hold).
    pub amount_micros: i64,
    /// The provider `ait1…` payee the captured funds settle to. The transport is
    /// responsible for resolving it to an `acct_…` before the capture settles
    /// (the real client via `GET /v1/addresses/:address`).
    pub payee: String,
    /// Same deterministic idem base used to open the hold (client suffixes it).
    pub idem_key: String,
}

/// Result of a capture.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureReceipt {
    /// Amount actually captured, in µ$.
    pub captured_micros: i64,
    /// Payer available balance after settle, in µ$ (for the receipt view).
    pub balance_after_micros: i64,
    /// Ledger sequence number of the settlement (receipt anchor).
    pub settlement_seq: u64,
    /// Signed, hash-linked Mint evidence for this exact terminal operation.
    pub proof: SettlementProof,
}

/// Normalized subset of Mint's `AITP-Receipt` persisted by spending products.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementProof {
    pub sequence: u64,
    pub operation: String,
    pub operation_hash: String,
    pub receipt_hash: String,
    pub signature: String,
}

/// Release an unused hold (any non-2xx / aborted stream / panic / timeout path).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    /// Same deterministic idem base used to open the hold (client suffixes it).
    pub idem_key: String,
}

/// Result of a release.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseReceipt {
    /// Amount released back to available balance, in µ$.
    pub released_micros: i64,
    /// Signed, hash-linked proof that the hold was released without charging.
    pub proof: SettlementProof,
}

/// A read of an agent's delegated budget allowance (the ceiling path).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Allowance {
    /// Allowance id (`alw_…`).
    pub allowance_id: String,
    /// The granted ceiling for the window, in µ$.
    pub limit_micros: i64,
    /// Already-spent amount in the current window, in µ$.
    pub spent_micros: i64,
    /// Whether the allowance has been revoked.
    pub revoked: bool,
}

impl Allowance {
    /// Remaining spendable headroom in the window, in µ$ (never negative).
    pub fn remaining_micros(&self) -> i64 {
        (self.limit_micros - self.spent_micros).max(0)
    }
}

/// Spend against an allowance under the hold-backed scheme: the response carries
/// a `hold_id` the caller later captures/releases like a direct hold.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllowanceSpend {
    /// Allowance to draw against.
    pub allowance_id: String,
    /// Amount to reserve (`est_max`), in µ$.
    pub amount_micros: i64,
    /// The provider `ait1…` payee the eventual capture settles to (resolved to an
    /// `acct_…` by the transport).
    pub payee: String,
    /// Deterministic idempotency key.
    pub idem_key: String,
}

/// Result of a hold-backed allowance spend.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllowanceSpendReceipt {
    /// The backing hold id, present under the hold-backed scheme.
    pub hold_id: Option<String>,
    /// Remaining allowance headroom after the reservation, in µ$.
    pub remaining_micros: i64,
    /// Ledger settlement sequence.
    pub settlement_seq: u64,
}

/// The Mint data-plane seam. Object-safe (async_trait boxes the futures) so it
/// can be stored as `Arc<dyn MintTransport>` in the server state.
#[async_trait]
pub trait MintTransport: Send + Sync {
    /// `POST /v1/holds` — reserve `amount_micros` against `account_id`.
    async fn create_hold(&self, req: Hold) -> Result<HoldReceipt, AbacusError>;
    /// `POST /v1/holds/:id/capture` — capture the exact actual cost to the payee.
    async fn capture_hold(
        &self,
        hold_id: &str,
        req: Capture,
    ) -> Result<CaptureReceipt, AbacusError>;
    /// `POST /v1/holds/:id/release` — release the unused hold.
    async fn release_hold(
        &self,
        hold_id: &str,
        req: Release,
    ) -> Result<ReleaseReceipt, AbacusError>;
    /// `GET /v1/allowances/:id` — read the delegated budget ceiling.
    async fn get_allowance(&self, allowance_id: &str) -> Result<Allowance, AbacusError>;
    /// `POST /v1/allowances/:id/spend` — hold-backed spend against the ceiling.
    async fn spend_allowance(
        &self,
        req: AllowanceSpend,
    ) -> Result<AllowanceSpendReceipt, AbacusError>;
}

// ---------------------------------------------------------------------------
// Upstream transport — the provider forwarder
// ---------------------------------------------------------------------------

/// A request forwarded to the real provider. `body` is the OpenAI-compatible /
/// tool payload passed through verbatim.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamRequest {
    /// Path appended to the configured provider base URL, e.g. `/v1/chat/completions`.
    pub path: String,
    /// Upstream bearer credential for the provider.
    pub bearer: String,
    /// The provider-shaped request body, forwarded verbatim.
    pub body: serde_json::Value,
}

/// Normalized token usage. Populated from either OpenAI keys
/// (`prompt_tokens`/`completion_tokens`) or native (`input_tokens`/`output_tokens`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// Input / prompt tokens.
    pub input_tokens: i64,
    /// Output / completion tokens.
    pub output_tokens: i64,
}

/// A provider response: upstream status, the body, and parsed usage (if any).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpstreamResponse {
    /// Upstream HTTP status.
    pub status: u16,
    /// The response body (non-streaming JSON, or the assembled SSE payload).
    pub body: serde_json::Value,
    /// Parsed terminal usage, if present. `None` for no-usage responses.
    pub usage: Option<Usage>,
}

impl UpstreamResponse {
    /// `true` iff the upstream returned a 2xx status.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// The provider forwarder seam. Object-safe for `Arc<dyn UpstreamTransport>`.
#[async_trait]
pub trait UpstreamTransport: Send + Sync {
    /// Forward `req` to the real provider and return its response + usage.
    async fn send(&self, req: UpstreamRequest) -> Result<UpstreamResponse, AbacusError>;
}

/// Parse a `usage` object from a provider response body, accepting BOTH the
/// OpenAI keys (`prompt_tokens`/`completion_tokens`) and the native keys
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
    })
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
                output_tokens: 5
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
                output_tokens: 7
            })
        );
    }

    #[test]
    fn null_usage_is_none() {
        assert_eq!(parse_usage(&json!({"usage": null})), None);
        assert_eq!(parse_usage(&json!({})), None);
    }

    #[test]
    fn allowance_remaining_never_negative() {
        let a = Allowance {
            allowance_id: "alw_x".into(),
            limit_micros: 1_000,
            spent_micros: 1_500,
            revoked: false,
        };
        assert_eq!(a.remaining_micros(), 0);
    }
}
