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
//!
//! ## Idempotent replay vs. the ownership guard
//! Single-owner goods (a cosmetic, a battle-pass) carry a *pre-hold* ownership
//! guard so a NEW purchase of something already owned is rejected before any
//! charge. A genuine idempotent RETRY (same `idem_key`, e.g. after a client
//! timeout on a call that actually succeeded) must NOT trip that guard — it has
//! to return the ORIGINAL outcome, exactly like a consumable pack does. The store
//! therefore memoizes every completed purchase by its `idem_key` and short-circuits
//! a replay to the stored outcome BEFORE the ownership guard runs. Without this,
//! retrying a settled cosmetic/pass purchase wrongly returns `AlreadyOwned` even
//! though Mint already captured the money — pushing the caller toward a fresh key
//! and a genuine double charge.

use std::sync::{Arc, RwLock};

use async_trait::async_trait;

use crate::catalog::{Catalog, CatalogError, Product};
use crate::catalog_repo::CatalogRepository;
use crate::entitlement::{EntitlementId, Inventory, OwnedPack};
use crate::error::GameError;
use crate::pack::{content_item_id, derive_pack_contents, pack_is_openable, GrantedItem};
use crate::state::{
    CompletedPurchase, GameStateRepository, MemoryGameState, PurchaseRecord, PurchaseReversal,
    ReversalKind, ReverseOutcome,
};
use crate::transport::{Capture, Hold, MintTransport, Refund, Release, SettlementProof};

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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// Full signed Mint evidence; this is the provenance root for the grant.
    pub settlement_proof: SettlementProof,
    /// `true` when this purchase was an idempotent replay (no new charge, no new
    /// entitlement — the same result as the original call).
    pub idempotent_replay: bool,
}

/// The outcome of [`GameStore::refund`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RefundOutcome {
    /// The purchase, now carrying its `refunded` reversal record.
    pub purchase: CompletedPurchase,
    /// `true` when the purchase had already been refunded (no second payout).
    pub idempotent_replay: bool,
}

/// The current catalog snapshot and the persisted revision it came from.
struct LiveCatalog {
    catalog: Arc<Catalog>,
    revision: u64,
}

impl LiveCatalog {
    fn new(catalog: Catalog) -> RwLock<Self> {
        RwLock::new(LiveCatalog {
            catalog: Arc::new(catalog),
            revision: 0,
        })
    }
}

/// A storefront bound to one Mint account rail. Generic over the transport `M`
/// and the fulfiller `F` so tests inject a FakeMint / a failing fulfiller.
pub struct GameStore<M: MintTransport, F: Fulfiller = AlwaysFulfill> {
    mint: M,
    fulfiller: F,
    /// The live catalog snapshot. Readers clone the `Arc` (cheap) and work on
    /// an immutable snapshot; operator edits build a new validated catalog and
    /// swap it in atomically.
    catalog: RwLock<LiveCatalog>,
    /// Where catalog edits are persisted. `None` keeps edits in memory only.
    catalog_repo: Option<Arc<dyn CatalogRepository>>,
    /// Serializes catalog edits within this process (the repository's revision
    /// check serializes them across instances).
    catalog_edit_lock: std::sync::Mutex<()>,
    /// The revenue account captures settle to.
    revenue_account: String,
    /// Durable purchase + entitlement projection. A pending intent is written
    /// before Mint is touched so the exact idempotent flow can recover after a
    /// crash between capture and local grant.
    state: Arc<dyn GameStateRepository>,
    /// Serializes post-purchase inventory mutations (pack open, pass advance).
    /// Those are read-modify-write over the state port; without this two
    /// concurrent opens of the same pack could both pass the `is_opened` check.
    inventory_lock: std::sync::Mutex<()>,
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
            catalog: LiveCatalog::new(catalog),
            catalog_repo: None,
            catalog_edit_lock: std::sync::Mutex::new(()),
            revenue_account: revenue_account.into(),
            state: Arc::new(MemoryGameState::default()),
            inventory_lock: std::sync::Mutex::new(()),
        }
    }

    /// Use an explicit durable state adapter (for example the JSON file adapter
    /// in `game-economy-store`).
    pub fn with_state(
        mint: M,
        fulfiller: F,
        catalog: Catalog,
        revenue_account: impl Into<String>,
        state: Arc<dyn GameStateRepository>,
    ) -> Self {
        Self {
            mint,
            fulfiller,
            catalog: LiveCatalog::new(catalog),
            catalog_repo: None,
            catalog_edit_lock: std::sync::Mutex::new(()),
            revenue_account: revenue_account.into(),
            state,
            inventory_lock: std::sync::Mutex::new(()),
        }
    }

    /// Persist catalog edits through `repo`. `revision` is the revision the
    /// store's current catalog was loaded at (`0` if it has never been saved).
    pub fn with_catalog_repository(
        mut self,
        repo: Arc<dyn CatalogRepository>,
        revision: u64,
    ) -> Self {
        self.catalog_repo = Some(repo);
        if let Ok(live) = self.catalog.get_mut() {
            live.revision = revision;
        }
        self
    }

    /// The current catalog snapshot. Cheap (an `Arc` clone); the snapshot never
    /// changes under the caller even if an operator edits the catalog.
    pub fn catalog(&self) -> Arc<Catalog> {
        match self.catalog.read() {
            Ok(live) => Arc::clone(&live.catalog),
            Err(poisoned) => Arc::clone(&poisoned.into_inner().catalog),
        }
    }

    /// The persisted revision of the current catalog snapshot.
    pub fn catalog_revision(&self) -> u64 {
        match self.catalog.read() {
            Ok(live) => live.revision,
            Err(poisoned) => poisoned.into_inner().revision,
        }
    }

    /// Apply an operator edit to the catalog.
    ///
    /// `edit` receives the current snapshot and returns the next one (the
    /// `Catalog::with_*` methods, which re-validate the whole catalog). The new
    /// catalog is persisted with an optimistic revision check and only then
    /// swapped in, so a failed save never leaves the storefront serving an
    /// unsaved catalog. If `expected_revision` is given and does not match the
    /// live revision the edit is rejected as a conflict (the operator was looking
    /// at a stale catalog).
    pub fn edit_catalog(
        &self,
        expected_revision: Option<u64>,
        edit: impl FnOnce(&Catalog) -> Result<Catalog, CatalogError>,
    ) -> Result<(Arc<Catalog>, u64), GameError> {
        let _edit = self
            .catalog_edit_lock
            .lock()
            .map_err(|_| GameError::Storage("catalog edit lock poisoned".into()))?;
        // Pick up edits made by other instances before building on the catalog.
        self.refresh_catalog()?;
        let (current, revision) = (self.catalog(), self.catalog_revision());
        if let Some(expected) = expected_revision {
            if expected != revision {
                return Err(GameError::CatalogConflict {
                    expected,
                    actual: revision,
                });
            }
        }
        let next = edit(&current).map_err(|e| GameError::InvalidCatalog(e.0))?;
        let new_revision = match &self.catalog_repo {
            Some(repo) => repo.save_catalog(&next.to_file(), revision)?,
            None => revision + 1,
        };
        let next = Arc::new(next);
        let mut live = self
            .catalog
            .write()
            .map_err(|_| GameError::Storage("catalog lock poisoned".into()))?;
        live.catalog = Arc::clone(&next);
        live.revision = new_revision;
        Ok((next, new_revision))
    }

    /// Reload the catalog from the repository when another instance saved a
    /// newer revision. Returns `true` when the live catalog changed.
    pub fn refresh_catalog(&self) -> Result<bool, GameError> {
        let Some(repo) = &self.catalog_repo else {
            return Ok(false);
        };
        let Some(stored) = repo.load_catalog()? else {
            return Ok(false);
        };
        if stored.revision <= self.catalog_revision() {
            return Ok(false);
        }
        let catalog =
            Catalog::from_file(stored.file).map_err(|e| GameError::InvalidCatalog(e.0))?;
        let mut live = self
            .catalog
            .write()
            .map_err(|_| GameError::Storage("catalog lock poisoned".into()))?;
        if stored.revision > live.revision {
            live.catalog = Arc::new(catalog);
            live.revision = stored.revision;
            return Ok(true);
        }
        Ok(false)
    }

    /// The Mint account captures settle to (and refunds are paid from).
    pub fn revenue_account(&self) -> &str {
        &self.revenue_account
    }

    /// The newest `limit` completed purchases across all players (operator view).
    pub fn recent_purchases(&self, limit: usize) -> Result<Vec<CompletedPurchase>, GameError> {
        self.state.recent_purchases(limit)
    }

    /// Store-wide purchase / refund totals (operator view).
    pub fn summary(&self) -> Result<crate::state::PurchaseSummary, GameError> {
        self.state.summary()
    }

    /// Borrow the underlying Mint transport (for inspection / test assertions).
    pub fn mint(&self) -> &M {
        &self.mint
    }

    /// A snapshot copy of a player's inventory (empty if the player is unknown).
    pub fn inventory(&self, player: &str) -> Inventory {
        self.try_inventory(player).unwrap_or_default()
    }

    /// Fallible inventory read for production callers that must surface storage
    /// failures rather than treating them as an empty account.
    pub fn try_inventory(&self, player: &str) -> Result<Inventory, GameError> {
        self.state.inventory(player)
    }

    /// A player's completed purchases (the receipt history), newest first.
    pub fn purchase_history(&self, player: &str) -> Result<Vec<CompletedPurchase>, GameError> {
        self.state.purchases_for(player)
    }

    /// Look up one of `player`'s completed purchases by its Mint receipt id.
    pub fn receipt(
        &self,
        player: &str,
        receipt_id: &str,
    ) -> Result<Option<CompletedPurchase>, GameError> {
        Ok(self
            .state
            .purchases_for(player)?
            .into_iter()
            .find(|p| p.outcome.receipt_id == receipt_id))
    }

    fn lock_inventory(&self) -> Result<std::sync::MutexGuard<'_, ()>, GameError> {
        self.inventory_lock
            .lock()
            .map_err(|_| GameError::Storage("inventory lock poisoned".into()))
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
        let catalog = self.catalog();
        let product = catalog
            .get(product_sku)
            .cloned()
            .ok_or_else(|| GameError::UnknownProduct(product_sku.to_string()))?;
        let price = product.price_micros();
        let entitlement_id = entitlement_id(player, idem_key);

        // 0. Idempotent replay: a completed purchase under this key returns the
        //    ORIGINAL outcome (no second charge, no duplicate grant). This runs
        //    BEFORE the ownership guard so a genuine retry of a single-owner good
        //    replays instead of being rejected as `AlreadyOwned`. Reusing the same
        //    key for a different player or SKU is an idempotency conflict.
        if let Some(record) = self.state.purchase(idem_key)? {
            match record {
                PurchaseRecord::Completed(done) => {
                    if done.player != player || done.outcome.product_sku != product_sku {
                        return Err(GameError::IdempotencyConflict);
                    }
                    let mut out = done.outcome;
                    out.idempotent_replay = true;
                    return Ok(out);
                }
                PurchaseRecord::Pending {
                    player: p,
                    product_sku: s,
                } if p != player || s != product_sku => {
                    return Err(GameError::IdempotencyConflict);
                }
                PurchaseRecord::Pending { .. } => {
                    // A previous process may have stopped at any network
                    // boundary. Replaying the deterministic Mint keys below is
                    // the recovery protocol.
                }
            }
        }

        // Archived products stay resolvable for owned entitlements (and for the
        // replay above) but can no longer be bought.
        if catalog.is_archived(product_sku) {
            return Err(GameError::ProductArchived(product_sku.to_string()));
        }

        // Persist intent before reserving money. The state adapter enforces
        // same-key/same-principal/same-SKU reuse.
        self.state.begin_purchase(idem_key, player, product_sku)?;

        // Pre-hold ownership guard for single-owner goods: never open a hold (let
        // alone charge) for a cosmetic/pass the player already owns. Packs are
        // consumable, so buying another is always allowed. Idempotent retries are
        // handled above, so this only fires for a genuinely NEW purchase. This
        // read of the PERSISTED inventory catches the already-granted (incl.
        // cross-restart) case; the atomic reservation below closes the
        // concurrent-in-flight race the read-then-act guard alone cannot.
        self.reject_if_already_owned(player, &product)?;

        // Atomically reserve single-owner goods BEFORE any money moves. Two
        // concurrent purchases (distinct idem keys) of the same cosmetic/pass now
        // serialize on the state lock: exactly one claims the reservation, the
        // other is rejected `AlreadyOwned` before a hold is ever opened. A genuine
        // same-key retry re-takes its own reservation (idempotent). Packs are
        // consumable, so they never reserve.
        let reservation = single_owner_key(&product);
        if let Some((ref ownership_key, ref sku)) = reservation {
            self.state
                .reserve_single_owner(player, ownership_key, sku, idem_key)?;
        }

        // 1. Reserve the price. Insufficient funds fails here — no hold, no grant.
        //    If the hold fails we must free the single-owner reservation so a
        //    fresh, funded attempt (a NEW key) can still buy the good.
        let hold = match self
            .mint
            .create_hold(Hold {
                account_id: player.to_string(),
                amount_micros: price,
                idem_key: format!("{idem_key}:hold"),
            })
            .await
        {
            Ok(hold) => hold,
            Err(err) => {
                if let Some((ref ownership_key, _)) = reservation {
                    self.state
                        .release_single_owner(player, ownership_key, idem_key);
                }
                return Err(err);
            }
        };

        // 2. Attempt delivery. Failure releases the hold AND the single-owner
        //    reservation (nothing was granted) and grants nothing.
        if let Err(fulfill_err) = self.fulfiller.fulfill(player, &product).await {
            let released = self
                .mint
                .release_hold(
                    &hold.hold_id,
                    Release {
                        idem_key: format!("{idem_key}:release"),
                    },
                )
                .await;
            if let Some((ref ownership_key, _)) = reservation {
                self.state
                    .release_single_owner(player, ownership_key, idem_key);
            }
            released?;
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

        // 4. Grant the entitlement (idempotent by deterministic id). Held under
        //    the in-process inventory lock (no awaits follow) and applied inside
        //    the state adapter's atomic commit, so a concurrent pack open or pass
        //    advance — on this or another instance — cannot clobber it.
        let _guard = self.lock_inventory()?;
        let receipt_id = capture.receipt_id.clone();
        let settlement_seq = capture.settlement_seq;

        let entitlement_id_for_grant = entitlement_id.clone();
        let outcome = PurchaseOutcome {
            entitlement_id,
            product_sku: product_sku.to_string(),
            charged_micros: capture.captured_micros,
            receipt_id: capture.receipt_id,
            settlement_seq: capture.settlement_seq,
            settlement_proof: capture.proof,
            idempotent_replay: capture.idempotent_replay,
        };

        // 5. Memoize the completed purchase so a later retry under the same key
        //    replays this exact outcome (see step 0) instead of re-charging or
        //    tripping the ownership guard.
        self.state.commit_purchase_with(
            idem_key,
            &CompletedPurchase::new(player, outcome.clone()),
            &|inventory: &mut Inventory| {
                grant_into(inventory, &product, &entitlement_id_for_grant, &receipt_id, settlement_seq)
            },
        )?;

        Ok(outcome)
    }

    /// Open an owned pack. Deterministic: contents are derived from the pack's
    /// stored purchase receipt id + `seed`. The `seed` MUST come from the
    /// server (see `game-economy-server`'s keyed pack-seed derivation), never
    /// from the player — a player-chosen seed can be ground offline for drops. A pack opens exactly once; each drop
    /// also unlocks its content-addressed cosmetic in the player's inventory.
    pub fn open_pack(
        &self,
        player: &str,
        entitlement_id: &str,
        seed: u64,
    ) -> Result<Vec<GrantedItem>, GameError> {
        let catalog = self.catalog();
        let _guard = self.lock_inventory()?;
        let mut opened: Vec<GrantedItem> = Vec::new();
        self.state.update_inventory(player, &mut |inv: &mut Inventory| {
            let pack = inv
                .packs
                .get(entitlement_id)
                .cloned()
                .ok_or_else(|| GameError::PackNotOwned(entitlement_id.to_string()))?;
            if pack.is_opened() {
                return Err(GameError::PackAlreadyOpened(entitlement_id.to_string()));
            }
            // Packs bought after runtime catalog editing carry a snapshot of the
            // tier they were sold under, so an operator changing the odds later
            // never changes what an already-paid-for pack can roll.
            let tier = match (&pack.tier, catalog.get(&pack.pack_sku)) {
                (Some(snapshot), _) => snapshot.clone(),
                (None, Some(Product::CardPack(t))) => t.clone(),
                _ => return Err(GameError::UnknownProduct(pack.pack_sku.clone())),
            };
            if !pack_is_openable(&tier, &catalog) {
                return Err(GameError::InvalidCatalog(format!(
                    "pack `{}` has no item pool for any rarity it can roll",
                    pack.pack_sku
                )));
            }
            let contents = derive_pack_contents(&tier, &catalog, &pack.receipt_id, seed);
            // Materialize + unlock each drop's cosmetic (idempotent).
            if let Some(p) = inv.packs.get_mut(entitlement_id) {
                p.contents = Some(contents.clone());
            }
            for item in &contents {
                inv.grant_cosmetic(&item.item_id, &item.name);
            }
            opened = contents;
            Ok(())
        })?;
        Ok(opened)
    }

    /// Advance a player's owned battle-pass to `target` tier.
    ///
    /// This is the post-purchase progression path: the game backend reports the
    /// tier a player has earned (through play), and the store clamps it to
    /// `[current, season_tier_count]` and persists it. It moves NO money — a pass
    /// is bought once on the Mint rail (see [`Self::purchase`]); tiers unlock for
    /// free as the season is played. Monotonic + saturating, so a replayed or
    /// stale report is a harmless no-op. Errors if the player does not own the
    /// pass or the SKU is not a battle-pass in the catalog.
    pub fn advance_battle_pass(
        &self,
        player: &str,
        pass_sku: &str,
        target: u32,
    ) -> Result<u32, GameError> {
        let max_tiers = match self.catalog().get(pass_sku) {
            Some(Product::BattlePass(b)) => b.tiers,
            Some(_) => return Err(GameError::UnknownProduct(pass_sku.to_string())),
            None => return Err(GameError::UnknownProduct(pass_sku.to_string())),
        };

        let _guard = self.lock_inventory()?;
        let mut tier = 0;
        self.state.update_inventory(player, &mut |inv: &mut Inventory| {
            let pass = inv
                .battle_pass
                .as_mut()
                .filter(|bp| bp.pass_sku == pass_sku)
                .ok_or_else(|| GameError::BattlePassNotOwned(pass_sku.to_string()))?;
            tier = pass.advance_to(target, max_tiers);
            Ok(())
        })?;
        Ok(tier)
    }

    /// Revoke the entitlement granted by `player`'s purchase `receipt_id`
    /// WITHOUT moving money — the chargeback path, where the card network has
    /// already reversed the payment outside Mint. Removes the pack (and any
    /// cosmetics it dropped), the cosmetic, or the battle pass, and marks the
    /// purchase `revoked` so it cannot be refunded afterwards. Use
    /// [`Self::refund`] to return money through Mint instead.
    pub fn revoke_entitlement(
        &self,
        player: &str,
        receipt_id: &str,
        reason: Option<&str>,
    ) -> Result<ReverseOutcome, GameError> {
        let _guard = self.lock_inventory()?;
        let ownership_key = self.ownership_key_for_receipt(player, receipt_id)?;
        let reversal = PurchaseReversal {
            kind: ReversalKind::Revoked,
            at_unix: now_unix(),
            refunded_micros: 0,
            mint_seq: None,
            mint_receipt: None,
            reason: reason.map(str::to_string),
        };
        self.state.reverse_purchase(
            player,
            receipt_id,
            &reversal,
            ownership_key.as_deref(),
            &|purchase, inv| self.remove_grant(purchase, inv),
        )
    }

    /// Refund `player`'s purchase `receipt_id` through Mint and remove what it
    /// granted.
    ///
    /// 1. The purchase must exist and not be reversed yet (a revoked purchase is
    ///    [`GameError::AlreadyReversed`]; an already-refunded one replays).
    /// 2. Mint transfers the captured amount from the revenue account back to
    ///    the player under the deterministic key `<receipt_id>:refund`, so a
    ///    retried or concurrent refund can never pay out twice.
    /// 3. The reversal and the entitlement removal commit atomically in the
    ///    state adapter.
    pub async fn refund(
        &self,
        player: &str,
        receipt_id: &str,
        reason: Option<&str>,
    ) -> Result<RefundOutcome, GameError> {
        let purchase = self
            .receipt(player, receipt_id)?
            .ok_or_else(|| GameError::PurchaseNotFound(receipt_id.to_string()))?;
        match purchase.reversal.as_ref().map(|r| r.kind) {
            Some(ReversalKind::Refunded) => {
                return Ok(RefundOutcome {
                    purchase,
                    idempotent_replay: true,
                })
            }
            Some(ReversalKind::Revoked) => {
                return Err(GameError::AlreadyReversed(receipt_id.to_string()))
            }
            None => {}
        }

        let amount = purchase.outcome.charged_micros;
        let (mint_seq, mint_receipt) = if amount > 0 {
            let receipt = self
                .mint
                .refund(Refund {
                    from: self.revenue_account.clone(),
                    to: player.to_string(),
                    amount_micros: amount,
                    idem_key: format!("{receipt_id}:refund"),
                    reference: Some(format!("game-refund:{receipt_id}")),
                })
                .await?;
            (Some(receipt.settlement_seq), receipt.receipt_hash)
        } else {
            (None, None)
        };

        let reversal = PurchaseReversal {
            kind: ReversalKind::Refunded,
            at_unix: now_unix(),
            refunded_micros: amount.max(0),
            mint_seq,
            mint_receipt,
            reason: reason.map(str::to_string),
        };
        let _guard = self.lock_inventory()?;
        let ownership_key = single_owner_key_for_sku(&self.catalog(), &purchase.outcome.product_sku);
        match self.state.reverse_purchase(
            player,
            receipt_id,
            &reversal,
            ownership_key.as_deref(),
            &|purchase, inv| self.remove_grant(purchase, inv),
        )? {
            ReverseOutcome::Reversed(purchase) => Ok(RefundOutcome {
                purchase,
                idempotent_replay: false,
            }),
            ReverseOutcome::AlreadyReversed(existing) => match existing.reversal.as_ref().map(|r| r.kind) {
                // A concurrent refund of the same receipt won the race; Mint
                // collapsed both transfers under one key, so nothing was paid twice.
                Some(ReversalKind::Refunded) => Ok(RefundOutcome {
                    purchase: existing,
                    idempotent_replay: true,
                }),
                _ => Err(GameError::AlreadyReversed(receipt_id.to_string())),
            },
            ReverseOutcome::NotFound => Err(GameError::PurchaseNotFound(receipt_id.to_string())),
        }
    }

    fn ownership_key_for_receipt(
        &self,
        player: &str,
        receipt_id: &str,
    ) -> Result<Option<String>, GameError> {
        Ok(self
            .receipt(player, receipt_id)?
            .and_then(|p| single_owner_key_for_sku(&self.catalog(), &p.outcome.product_sku)))
    }

    /// Remove what `purchase` granted from `inv`.
    fn remove_grant(&self, purchase: &CompletedPurchase, inv: &mut Inventory) {
        match self.catalog().get(&purchase.outcome.product_sku) {
            Some(Product::CardPack(_)) | None => {
                if let Some(pack) = inv.packs.remove(&purchase.outcome.entitlement_id) {
                    for item in pack.contents.unwrap_or_default() {
                        inv.cosmetics.remove(&item.item_id);
                    }
                }
            }
            Some(Product::Cosmetic(c)) => {
                inv.cosmetics.remove(&content_item_id(c.rarity, &c.name));
            }
            Some(Product::BattlePass(b)) => {
                if inv.owns_battle_pass(&b.sku) {
                    inv.battle_pass = None;
                }
            }
        }
    }

    /// Reject a purchase of a single-owner good the player already holds, BEFORE
    /// any hold is opened (so a duplicate is never charged).
    fn reject_if_already_owned(&self, player: &str, product: &Product) -> Result<(), GameError> {
        let inv = self.state.inventory(player)?;
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

/// Grant the entitlement for a captured purchase into `inv`. Idempotent — a
/// replayed purchase re-derives the same entitlement id and no-ops.
fn grant_into(
    inv: &mut Inventory,
    product: &Product,
    entitlement_id: &str,
    receipt_id: &str,
    settlement_seq: u64,
) {
    match product {
        Product::CardPack(t) => inv.grant_pack(OwnedPack {
            entitlement_id: entitlement_id.to_string(),
            pack_sku: t.sku.clone(),
            settlement_seq,
            receipt_id: receipt_id.to_string(),
            contents: None,
            tier: Some(t.clone()),
        }),
        Product::Cosmetic(c) => {
            let item_id = content_item_id(c.rarity, &c.name);
            inv.grant_cosmetic(&item_id, &c.name);
        }
        Product::BattlePass(b) => inv.grant_battle_pass(&b.sku),
    }
}

fn single_owner_key_for_sku(catalog: &Catalog, sku: &str) -> Option<String> {
    catalog
        .get(sku)
        .and_then(single_owner_key)
        .map(|(ownership_key, _)| ownership_key)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The `(ownership_key, sku)` a single-owner good is reserved under, or `None`
/// for consumable packs (which never reserve). The `ownership_key` is namespaced
/// per shape and matches exactly what [`GameStore::reject_if_already_owned`]
/// checks against the persisted inventory, so the concurrency reservation and the
/// persisted-ownership guard agree on identity.
pub fn single_owner_key(product: &Product) -> Option<(String, String)> {
    match product {
        Product::Cosmetic(c) => {
            let item_id = content_item_id(c.rarity, &c.name);
            Some((format!("cosmetic:{item_id}"), c.sku.clone()))
        }
        Product::BattlePass(b) => Some((format!("pass:{}", b.sku), b.sku.clone())),
        Product::CardPack(_) => None,
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
