//! The Mint data-plane seam — the direct holds path only.
//!
//! This is a faithful port of `abacus-core::transport::MintTransport`, trimmed to
//! the three calls a fixed-price storefront needs: `create_hold` reserves the
//! price, `capture_hold` settles it to the revenue account (terminal, once), and
//! `release_hold` frees an unfulfilled reservation. The trait lives HERE so the
//! store depends on the abstraction, not on HTTP: the real client would map these
//! to `POST /v1/holds`, `POST /v1/holds/:id/capture`, `POST /v1/holds/:id/release`
//! and the tests inject the in-memory `FakeMint`. All money is bare `i64` µ$.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::GameError;

/// Open a reserve hold on Mint. `idem_key` is deterministic per purchase; the
/// store applies the `:hold` / `:capture` / `:release` suffixes across the triple
/// (identical to the abacus mint-client convention) so retries collapse in Mint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hold {
    /// Mint account the hold is placed against (the player's funding account).
    pub account_id: String,
    /// The exact price to reserve, in µ$.
    pub amount_micros: i64,
    /// Deterministic idempotency key for the create leg.
    pub idem_key: String,
}

/// A successfully opened hold.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HoldReceipt {
    /// Mint hold id (`hold_…`).
    pub hold_id: String,
    /// The reserved amount, in µ$.
    pub amount_micros: i64,
    /// `true` when this was an idempotent replay (no new reservation opened).
    pub idempotent_replay: bool,
}

/// Capture the price against an existing hold, settling it to the revenue payee.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capture {
    /// Amount to capture, in µ$ (`<=` the hold; for a fixed-price good == price).
    pub amount_micros: i64,
    /// The revenue account the captured funds settle to (`acct_…`).
    pub to: String,
    /// Deterministic idempotency key for the capture leg.
    pub idem_key: String,
}

/// Result of a capture — the receipt that backs the entitlement grant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptureReceipt {
    /// Amount actually captured, in µ$.
    pub captured_micros: i64,
    /// Payer available balance after settle, in µ$.
    pub balance_after_micros: i64,
    /// Ledger sequence number of the settlement (receipt anchor).
    pub settlement_seq: u64,
    /// Content-addressed receipt id (`rcpt_…`) — the auditable anchor a pack's
    /// contents are derived from.
    pub receipt_id: String,
    /// `true` when this was an idempotent replay (funds already captured once).
    pub idempotent_replay: bool,
}

/// Release an unfulfilled hold (grants nothing, charges nothing).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    /// Deterministic idempotency key for the release leg.
    pub idem_key: String,
}

/// Result of a release.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseReceipt {
    /// Amount released back to available balance, in µ$.
    pub released_micros: i64,
    /// `true` when this was an idempotent replay.
    pub idempotent_replay: bool,
}

/// The Mint holds seam. Object-safe (async_trait boxes the futures) so a store
/// can hold an `Arc<dyn MintTransport>`.
#[async_trait]
pub trait MintTransport: Send + Sync {
    /// `POST /v1/holds` — reserve `amount_micros` against `account_id`.
    async fn create_hold(&self, req: Hold) -> Result<HoldReceipt, GameError>;

    /// `POST /v1/holds/:id/capture` — settle the price to the revenue account.
    async fn capture_hold(
        &self,
        hold_id: &str,
        req: Capture,
    ) -> Result<CaptureReceipt, GameError>;

    /// `POST /v1/holds/:id/release` — free the unfulfilled reservation.
    async fn release_hold(
        &self,
        hold_id: &str,
        req: Release,
    ) -> Result<ReleaseReceipt, GameError>;
}
