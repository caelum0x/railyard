//! The Mint data-plane seam — the direct holds path only.
//!
//! This is a faithful port of `abacus-core::transport::MintTransport`, trimmed to
//! the calls a fixed-price storefront needs: `create_hold` reserves the
//! price, `capture_hold` settles it to the revenue account (terminal, once), and
//! `release_hold` frees an unfulfilled reservation. The trait lives HERE so the
//! store depends on the abstraction, not on HTTP: the real client would map these
//! to `POST /v1/holds`, `POST /v1/holds/:id/capture`, `POST /v1/holds/:id/release`
//! and the tests inject the in-memory `FakeMint`. All money is bare `i64` µ$.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::GameError;

/// Authoritative proof emitted and signed by Mint for a terminal hold operation.
/// Entitlements persist this whole projection, not a locally synthesized id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementProof {
    pub sequence: u64,
    pub operation: String,
    pub operation_hash: String,
    pub receipt_hash: String,
    pub signature: String,
}

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
    /// Signed Mint evidence backing this grant.
    pub proof: SettlementProof,
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
    /// Signed Mint evidence proving the reservation was released.
    pub proof: SettlementProof,
}

/// Refund a settled purchase: a direct Mint transfer from the storefront's
/// revenue account back to the player's funding account. `idem_key` is derived
/// from the purchase receipt (`<receipt_id>:refund`), so a retried or concurrent
/// refund of the same receipt collapses in Mint and can never pay out twice.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Refund {
    /// The revenue account the original capture settled to (debited).
    pub from: String,
    /// The player's Mint account (credited).
    pub to: String,
    /// Amount to return, in µ$ (the originally captured amount).
    pub amount_micros: i64,
    /// Deterministic idempotency key for the refund transfer.
    pub idem_key: String,
    /// Free-form reference recorded on the Mint journal (e.g. the receipt id).
    pub reference: Option<String>,
}

/// Result of a refund transfer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefundReceipt {
    /// Amount moved back to the player, in µ$.
    pub refunded_micros: i64,
    /// Ledger sequence of the refund transfer (sender journal leg).
    pub settlement_seq: u64,
    /// Mint receipt hash of the refund transfer, when Mint returned one.
    pub receipt_hash: Option<String>,
    /// `true` when Mint reported this as an idempotent replay.
    pub idempotent_replay: bool,
}

/// A player's Mint balance, as read for display. `available_micros` is what the
/// player can spend right now; `held_micros` is reserved by in-flight purchases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountBalance {
    pub account_id: String,
    pub available_micros: i64,
    pub held_micros: i64,
}

/// The Mint holds seam. Object-safe (async_trait boxes the futures) so a store
/// can hold an `Arc<dyn MintTransport>`.
#[async_trait]
pub trait MintTransport: Send + Sync {
    /// `POST /v1/holds` — reserve `amount_micros` against `account_id`.
    async fn create_hold(&self, req: Hold) -> Result<HoldReceipt, GameError>;

    /// `POST /v1/holds/:id/capture` — settle the price to the revenue account.
    async fn capture_hold(&self, hold_id: &str, req: Capture) -> Result<CaptureReceipt, GameError>;

    /// `POST /v1/holds/:id/release` — free the unfulfilled reservation.
    async fn release_hold(&self, hold_id: &str, req: Release) -> Result<ReleaseReceipt, GameError>;

    /// `POST /v1/transfers` — return a captured purchase's money from the
    /// revenue account to the player (refund path).
    async fn refund(&self, req: Refund) -> Result<RefundReceipt, GameError>;

    /// `GET /v1/accounts/:id/balance` — read-only balance lookup. An account Mint
    /// has never seen reads as zero (a new player simply has nothing yet).
    async fn read_balance(&self, account_id: &str) -> Result<AccountBalance, GameError>;

    /// `POST /v1/accounts` `{kind:"User", owner_ref}` — open a new player account
    /// and return its Mint id (`acct_…`). Each call opens a fresh account.
    async fn create_player_account(&self, owner_ref: &str) -> Result<String, GameError>;
}
