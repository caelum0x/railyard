//! [`GameStore`] — the success-only purchase state machine over Mint.
//!
//! One rule, borrowed verbatim from Abacus/ADR-0002 and applied to game SKUs:
//!
//! ```text
//!   create_hold(price)                 reserve the exact price
//!        │
//!        ├── fulfillment OK  ──► capture_hold(price → revenue) ──► grant entitlement
//!        └── fulfillment FAIL ─► release_hold()                ──► grant nothing
//! ```
//!
//! Capture happens ONLY on successful fulfillment; every other terminal releases
//! the hold, so the player is never charged for something they didn't receive.
//! The whole triple reuses ONE deterministic `idem_key` (with `:hold`/`:capture`/
//! `:release` suffixes), so a retried purchase collapses in Mint and the grant is
//! re-derived to the SAME entitlement id — never a double charge, never a
//! duplicate item.

use std::collections::HashMap;
use std::sync::Mutex;

use async_trait::async_trait;

use crate::catalog::{Catalog, Product};
use crate::entitlement::{EntitlementId, Inventory, OwnedPack};
use crate::error::GameError;
use crate::pack::{content_item_id, derive_pack_contents, GrantedItem};
use crate::transport::{Capture, Hold, MintTransport, Release};

/// The post-authorization delivery seam. After the hold opens, the store asks the
/// fulfiller whether the good can actually be delivered (stock, region-lock, a
/// downstream mint of the asset, ...). `Ok` → capture; `Err` → release. This is
/// the exact analog of Abacus's upstream call deciding capture-vs-release.
#[async_trait]
pub trait Fulfiller: Send + Sync {
    /// Attempt to deliver `product` to `player`. An `Err` releases the hold.
    async fn fulfill(&self, player: &str, product: &Product) -> Result<(), GameError>;
}

/// The default fulfiller: digital goods always deliver.
#[derive(Debug, Clone, Copy, Default)]
pub struct AlwaysFulfill;

#[async_trait]
impl Fulfiller for AlwaysFulfill {
    async fn fulfill(&self, _player: &str, _product: &Product) -> Result<(), GameError> {
        Ok(())
    }
}

/// The outcome of a completed purchase.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurchaseOutcome {
    /// The granted entitlement id.
    pub entitlement_id: EntitlementId,
    /// The SKU that was purchased.
    pub product_sku: String,
    /// Amount actually captured (revenue), in µ$.
    pub charged_micros: i64,
    /// The content-addressed Mint receipt id backing the grant.
    pub receipt_id: String,
    /// The Mint settlement sequence.
    pub settlement_seq: u64,
    /// `true` when this purchase was an idempotent replay (no new charge, no new
    /// entitlement — the same result as the original call).
    pub idempotent_replay: bool,
}

/// A storefront bound to one Mint account rail. Generic over the transport `M`
/// and the fulfiller `F` so tests inject a FakeMint / a failing fulfiller.
pub struct GameStore<M: MintTransport, F: Fulfiller = AlwaysFulfill> {
    mint: M,
    fulfiller: F,
    catalog: Catalog,
    /// The revenue account captures settle to.
    revenue_account: String,
    /// player -> inventory.
    inventories: Mutex<HashMap<String, Inventory>>,
}

impl<M: MintTransport> GameStore<M, AlwaysFulfill> {
    /// A store with the default always-succeeds fulfiller.
    pub fn new(mint: M, catalog: Catalog, revenue_account: impl Into<String>) -> Self {
        GameStore::with_fulfiller(mint, AlwaysFulfill, catalog, revenue_account)
    }
}

impl<M: MintTransport, F: Fulfiller> GameStore<M, F> {
    /// A store with an explicit fulfiller (used to exercise the release path).
    pub fn with_fulfiller(
        mint: M,
        fulfiller: F,
        catalog: Catalog,
        revenue_account: impl Into<String>,
    ) -> Self {
        GameStore {
            mint,
            fulfiller,
            catalog,
            revenue_account: revenue_account.into(),
            inventories: Mutex::new(HashMap::new()),
        }
    }

    /// Borrow the catalog.
    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    /// Borrow the underlying Mint transport (for inspection / test assertions).
    pub fn mint(&self) -> &M {
        &self.mint
    }

    /// A snapshot copy of a player's inventory (empty if the player is unknown).
    pub fn inventory(&self, player: &str) -> Inventory {
        self.inventories
            .lock()
            .unwrap()
            .get(player)
            .cloned()
            .unwrap_or_default()
    }

    /// Buy `product_sku` for `player`, reserving and (on success) capturing the
    /// price on Mint, then granting the entitlement. See the module docs for the
    /// success-only state machine. Deterministic + idempotent in `idem_key`.
    pub async fn purchase(
        &self,
        player: &str,
        product_sku: &str,
        idem_key: &str,
    ) -> Result<PurchaseOutcome, GameError> {
        let product = self
            .catalog
            .get(product_sku)
            .cloned()
            .ok_or_else(|| GameError::UnknownProduct(product_sku.to_string()))?;
        let price = product.price_micros();
        let entitlement_id = entitlement_id(player, idem_key);

        // Pre-hold ownership guard for single-owner goods: never open a hold (let
        // alone charge) for a cosmetic/pass the player already owns. Packs are
        // consumable, so buying another is always allowed.
        self.reject_if_already_owned(player, &product)?;

        // 1. Reserve the price. Insufficient funds fails here — no hold, no grant.
        let hold = self
            .mint
            .create_hold(Hold {
                account_id: player.to_string(),
                amount_micros: price,
                idem_key: format!("{idem_key}:hold"),
            })
            .await?;

        // 2. Attempt delivery. Failure releases the hold and grants nothing.
        if let Err(fulfill_err) = self.fulfiller.fulfill(player, &product).await {
            self.mint
                .release_hold(
                    &hold.hold_id,
                    Release {
                        idem_key: format!("{idem_key}:release"),
                    },
                )
                .await?;
            return Err(fulfill_err);
        }

        // 3. Capture the price to revenue — terminal, once. The captured receipt
        //    id anchors the grant (and, for packs, the deterministic open roll).
        let capture = self
            .mint
            .capture_hold(
                &hold.hold_id,
                Capture {
                    amount_micros: price,
                    to: self.revenue_account.clone(),
                    idem_key: format!("{idem_key}:capture"),
                },
            )
            .await?;

        // 4. Grant the entitlement (idempotent by deterministic id).
        self.grant(player, &product, &entitlement_id, &capture.receipt_id, capture.settlement_seq);

        Ok(PurchaseOutcome {
            entitlement_id,
            product_sku: product_sku.to_string(),
            charged_micros: capture.captured_micros,
            receipt_id: capture.receipt_id,
            settlement_seq: capture.settlement_seq,
            idempotent_replay: capture.idempotent_replay,
        })
    }

    /// Open an owned pack. Deterministic: contents are derived from the pack's
    /// stored purchase receipt id + `seed`. A pack opens exactly once; each drop
    /// also unlocks its content-addressed cosmetic in the player's inventory.
    pub fn open_pack(
        &self,
        player: &str,
        entitlement_id: &str,
        seed: u64,
    ) -> Result<Vec<GrantedItem>, GameError> {
        let mut invs = self.inventories.lock().unwrap();
        let inv = invs
            .get_mut(player)
            .ok_or_else(|| GameError::PackNotOwned(entitlement_id.to_string()))?;
        let pack = inv
            .packs
            .get(entitlement_id)
            .cloned()
            .ok_or_else(|| GameError::PackNotOwned(entitlement_id.to_string()))?;
        if pack.is_opened() {
            return Err(GameError::PackAlreadyOpened(entitlement_id.to_string()));
        }

        let tier = match self.catalog.get(&pack.pack_sku) {
            Some(Product::CardPack(t)) => t.clone(),
            _ => return Err(GameError::UnknownProduct(pack.pack_sku.clone())),
        };

        let contents = derive_pack_contents(&tier, &self.catalog, &pack.receipt_id, seed);

        // Materialize + unlock each drop's cosmetic (idempotent).
        if let Some(p) = inv.packs.get_mut(entitlement_id) {
            p.contents = Some(contents.clone());
        }
        for item in &contents {
            inv.grant_cosmetic(&item.item_id, &item.name);
        }
        Ok(contents)
    }

    /// Grant the entitlement for a captured purchase. Idempotent — a replayed
    /// purchase re-derives the same entitlement id and no-ops.
    fn grant(
        &self,
        player: &str,
        product: &Product,
        entitlement_id: &str,
        receipt_id: &str,
        settlement_seq: u64,
    ) {
        let mut invs = self.inventories.lock().unwrap();
        let inv = invs.entry(player.to_string()).or_default();
        match product {
            Product::CardPack(t) => inv.grant_pack(OwnedPack {
                entitlement_id: entitlement_id.to_string(),
                pack_sku: t.sku.clone(),
                settlement_seq,
                receipt_id: receipt_id.to_string(),
                contents: None,
            }),
            Product::Cosmetic(c) => {
                let item_id = content_item_id(c.rarity, &c.name);
                inv.grant_cosmetic(&item_id, &c.name);
            }
            Product::BattlePass(b) => inv.grant_battle_pass(&b.sku),
        }
    }

    /// Reject a purchase of a single-owner good the player already holds, BEFORE
    /// any hold is opened (so a duplicate is never charged).
    fn reject_if_already_owned(&self, player: &str, product: &Product) -> Result<(), GameError> {
        let invs = self.inventories.lock().unwrap();
        let Some(inv) = invs.get(player) else {
            return Ok(());
        };
        match product {
            Product::Cosmetic(c) => {
                let item_id = content_item_id(c.rarity, &c.name);
                if inv.owns_cosmetic(&item_id) {
                    return Err(GameError::AlreadyOwned(c.sku.clone()));
                }
            }
            Product::BattlePass(b) => {
                if inv.owns_battle_pass(&b.sku) {
                    return Err(GameError::AlreadyOwned(b.sku.clone()));
                }
            }
            // Packs are consumable — always buyable.
            Product::CardPack(_) => {}
        }
        Ok(())
    }
}

/// The deterministic entitlement id for a `(player, idem_key)` pair. Because it is
/// a pure function of the idempotency key, a replayed purchase grants to the SAME
/// id, so re-granting is a no-op rather than a duplicate.
pub fn entitlement_id(player: &str, idem_key: &str) -> EntitlementId {
    let preimage = format!("ent:v1:{player}:{idem_key}");
    let hash = blake3::hash(preimage.as_bytes());
    format!("ent_{}", &hash.to_hex()[..24])
}
