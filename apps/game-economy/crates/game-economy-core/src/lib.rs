//! `game-economy-core` — a GAME storefront that settles on Railyard's Mint rail.
//!
//! Card packs (multiple price tiers), cosmetics/skins, and a battle-pass all
//! settle through the SAME hold → capture → receipt flow the rest of the Railyard
//! portfolio uses. The store reuses Mint's semantics verbatim through an
//! injectable [`transport::MintTransport`] seam (the real HTTP client in prod, a
//! faithful FakeMint in tests) — it never reimplements the ledger.
//!
//! Design in four pieces:
//! - [`catalog`]     — the typed SKU catalog with µ$ prices.
//! - [`transport`]   — the Mint holds seam (`create`/`capture`/`release`).
//! - [`store`]       — the success-only purchase state machine + entitlement grant.
//! - [`pack`]        — deterministic, receipt-anchored pack opening.
//! - [`entitlement`] — the per-player inventory of what was granted.

pub mod catalog;
pub mod catalog_repo;
pub mod entitlement;
pub mod error;
pub mod money;
pub mod pack;
pub mod state;
pub mod store;
pub mod transport;

pub use catalog::{
    BattlePass, CardPackTier, Catalog, CatalogError, CatalogFile, Cosmetic, Product, Rarity,
};
pub use entitlement::{BattlePassState, EntitlementId, Inventory, OwnedPack};
pub use error::GameError;
pub use pack::{content_item_id, derive_pack_contents, pack_is_openable, GrantedItem};
pub use catalog_repo::{CatalogRepository, MemoryCatalog, StoredCatalog};
pub use state::{
    completed_for, recent_of, reverse_in, summarize, CompletedPurchase, GameStateRepository, MemoryGameState,
    PurchaseRecord, PurchaseReversal, PurchaseSummary, ReversalKind, ReverseOutcome,
};
pub use store::{
    entitlement_id, single_owner_key, AlwaysFulfill, Fulfiller, GameStore, PurchaseOutcome,
    RefundOutcome,
};
pub use transport::{
    AccountBalance, Capture, CaptureReceipt, Hold, HoldReceipt, MintTransport, Refund, RefundReceipt, Release,
    ReleaseReceipt, SettlementProof,
};
