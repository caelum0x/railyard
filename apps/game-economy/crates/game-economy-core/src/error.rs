//! Errors surfaced by the game store. The Mint-settlement variants mirror the
//! ledger's own error vocabulary (`insufficient_funds`, `hold_not_found`,
//! `idempotency_conflict`, `capture_exceeds_hold`) so a failure on the rail is
//! reported with the same meaning it has in Mint.

use thiserror::Error;

/// A game-store failure.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum GameError {
    /// The requested SKU is not in the catalog.
    #[error("unknown product: {0}")]
    UnknownProduct(String),

    /// The player's available balance could not cover the hold (Mint `402`).
    #[error("insufficient funds")]
    InsufficientFunds,

    /// A capture/release targeted a hold that is missing or already terminal.
    /// (Mint collapses "distinct close of a terminal hold" to not-found.)
    #[error("hold not found")]
    HoldNotFound,

    /// Same idempotency key replayed with a different request body (Mint `409`).
    #[error("idempotency conflict")]
    IdempotencyConflict,

    /// Capture amount exceeded the reservation (Mint `422`).
    #[error("capture exceeds hold")]
    CaptureExceedsHold,

    /// Fulfillment failed AFTER the hold opened — the success-only path releases
    /// the hold and grants nothing. Carries a human-readable reason.
    #[error("fulfillment failed: {0}")]
    FulfillmentFailed(String),

    /// A cosmetic/battle-pass the player already owns (checked before any hold,
    /// so the player is never charged for a duplicate).
    #[error("already owned: {0}")]
    AlreadyOwned(String),

    /// Pack-opening was requested for an entitlement the player does not hold.
    #[error("pack not owned: {0}")]
    PackNotOwned(String),

    /// A pack can only be opened once; its contents are already materialized.
    #[error("pack already opened: {0}")]
    PackAlreadyOpened(String),

    /// A transport/ledger error with no more specific mapping.
    #[error("mint error: {0}")]
    Mint(String),
}
