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

    /// The SKU exists but has been archived by the operator: owned copies keep
    /// working, but it can no longer be bought.
    #[error("product unavailable: {0}")]
    ProductArchived(String),

    /// A catalog edit (or a pack open against the live catalog) failed
    /// validation.
    #[error("invalid catalog: {0}")]
    InvalidCatalog(String),

    /// A catalog save was based on a stale revision (another operator or
    /// instance edited first). Reload the catalog and retry the edit.
    #[error("catalog changed concurrently (expected revision {expected}, found {actual})")]
    CatalogConflict {
        /// Revision the edit was based on.
        expected: u64,
        /// Revision currently stored.
        actual: u64,
    },

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

    /// Tier progression was requested for a battle-pass SKU the player does not
    /// hold (a pass must be purchased before its tiers can advance).
    #[error("battle pass not owned: {0}")]
    BattlePassNotOwned(String),

    /// Pack-opening was requested for an entitlement the player does not hold.
    #[error("pack not owned: {0}")]
    PackNotOwned(String),

    /// A pack can only be opened once; its contents are already materialized.
    #[error("pack already opened: {0}")]
    PackAlreadyOpened(String),

    /// No completed purchase with this receipt id exists for the player.
    #[error("purchase not found: {0}")]
    PurchaseNotFound(String),

    /// The purchase was already refunded or revoked; it cannot be reversed twice.
    #[error("purchase already reversed: {0}")]
    AlreadyReversed(String),

    /// A transport/ledger error with no more specific mapping.
    #[error("mint error: {0}")]
    Mint(String),

    /// Durable entitlement/purchase projection failed.
    #[error("game state storage error: {0}")]
    Storage(String),
}
