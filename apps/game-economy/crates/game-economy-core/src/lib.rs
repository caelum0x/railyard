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
pub mod entitlement;
pub mod error;
pub mod money;
pub mod pack;
pub mod store;
pub mod transport;

pub use catalog::{BattlePass, CardPackTier, Catalog, Cosmetic, Product, Rarity};
pub use entitlement::{BattlePassState, EntitlementId, Inventory, OwnedPack};
pub use error::GameError;
pub use pack::{content_item_id, derive_pack_contents, GrantedItem};
pub use store::{entitlement_id, AlwaysFulfill, Fulfiller, GameStore, PurchaseOutcome};
pub use transport::{
    Capture, CaptureReceipt, Hold, HoldReceipt, MintTransport, Release, ReleaseReceipt,
};
