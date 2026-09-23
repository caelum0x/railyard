//! Durable state seam for purchases and player inventory.
//!
//! The Mint transaction and the entitlement database cannot share one database
//! transaction. Recovery therefore relies on a persisted purchase intent plus
//! Mint's deterministic idempotency keys: after a crash, replaying the same
//! purchase re-observes the same hold/capture receipt and atomically commits the
//! grant locally.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::{GameError, Inventory, PurchaseOutcome};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompletedPurchase {
    pub player: String,
    pub outcome: PurchaseOutcome,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PurchaseRecord {
    Pending { player: String, product_sku: String },
    Completed(CompletedPurchase),
}

/// Atomic local projection required by [`crate::GameStore`]. Production
/// adapters may use Postgres; the included JSON adapter lives in the sibling
/// `game-economy-store` crate.
pub trait GameStateRepository: Send + Sync {
    fn inventory(&self, player: &str) -> Result<Inventory, GameError>;
    fn purchase(&self, idem_key: &str) -> Result<Option<PurchaseRecord>, GameError>;
    /// Every COMPLETED purchase made by `player`, newest settlement first. Backs
    /// the player's purchase history / receipt lookup routes.
    fn purchases_for(&self, player: &str) -> Result<Vec<CompletedPurchase>, GameError>;
    fn begin_purchase(
        &self,
        idem_key: &str,
        player: &str,
        product_sku: &str,
    ) -> Result<(), GameError>;
    fn commit_purchase(
        &self,
        idem_key: &str,
        completed: &CompletedPurchase,
        inventory: &Inventory,
    ) -> Result<(), GameError>;
    fn save_inventory(&self, player: &str, inventory: &Inventory) -> Result<(), GameError>;

    /// Atomically claim a single-owner good (`ownership_key`) for `player` under
    /// the same lock that guards inventory, BEFORE any Mint hold is opened. This
    /// is the concurrency guard that makes the pre-hold ownership check
    /// check-and-reserve rather than read-then-act: two concurrent purchases of
    /// distinct idempotency keys for the same single-owner good can no longer both
    /// pass — exactly one wins, the other gets [`GameError::AlreadyOwned`].
    ///
    /// Contract:
    /// - No reservation held and not already granted → record it, return `Ok`.
    /// - Reservation already held by the SAME `idem_key` → `Ok` (idempotent retry,
    ///   e.g. recovering a pending purchase, must not deadlock itself).
    /// - Reservation held by a DIFFERENT key → `Err(AlreadyOwned(sku))`.
    ///
    /// Reservations are process-local (not persisted): they guard concurrent
    /// in-flight purchases within ONE server process and are cleared on
    /// [`GameStateRepository::release_single_owner`] (failure path) while a
    /// committed grant keeps the good out of contention via the persisted
    /// inventory. A horizontally-scaled deployment MUST back this with a database
    /// unique constraint / row lock instead — the JSON/in-memory adapters are
    /// honestly single-instance only.
    fn reserve_single_owner(
        &self,
        player: &str,
        ownership_key: &str,
        sku: &str,
        idem_key: &str,
    ) -> Result<(), GameError>;

    /// Release a single-owner reservation previously taken by `idem_key` (used on
    /// the fulfillment-failure / hold-not-opened path so a fresh purchase of the
    /// same good can proceed). A no-op unless the reservation is currently held by
    /// this exact `idem_key`.
    fn release_single_owner(&self, player: &str, ownership_key: &str, idem_key: &str);
}

#[derive(Default)]
pub struct MemoryGameState {
    inner: Mutex<MemorySnapshot>,
}

#[derive(Default)]
struct MemorySnapshot {
    inventories: HashMap<String, Inventory>,
    purchases: HashMap<String, PurchaseRecord>,
    /// In-flight single-owner reservations: `(player, ownership_key) -> idem_key`.
    /// Guarded by the same mutex as `inventories`, so check-and-reserve is atomic.
    reservations: HashMap<(String, String), String>,
}

impl GameStateRepository for MemoryGameState {
    fn inventory(&self, player: &str) -> Result<Inventory, GameError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| GameError::Storage("game state lock poisoned".into()))?
            .inventories
            .get(player)
            .cloned()
            .unwrap_or_default())
    }

    fn purchase(&self, idem_key: &str) -> Result<Option<PurchaseRecord>, GameError> {
        Ok(self
            .inner
            .lock()
            .map_err(|_| GameError::Storage("game state lock poisoned".into()))?
            .purchases
            .get(idem_key)
            .cloned())
    }

    fn purchases_for(&self, player: &str) -> Result<Vec<CompletedPurchase>, GameError> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| GameError::Storage("game state lock poisoned".into()))?;
        Ok(completed_for(guard.purchases.values(), player))
    }

    fn begin_purchase(
        &self,
        idem_key: &str,
        player: &str,
        product_sku: &str,
    ) -> Result<(), GameError> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| GameError::Storage("game state lock poisoned".into()))?;
        match state.purchases.get(idem_key) {
            None => {
                state.purchases.insert(
                    idem_key.into(),
                    PurchaseRecord::Pending {
                        player: player.into(),
                        product_sku: product_sku.into(),
                    },
                );
                Ok(())
            }
            Some(PurchaseRecord::Pending {
                player: p,
                product_sku: s,
            }) if p == player && s == product_sku => Ok(()),
            Some(PurchaseRecord::Completed(done))
                if done.player == player && done.outcome.product_sku == product_sku =>
            {
                Ok(())
            }
            Some(_) => Err(GameError::IdempotencyConflict),
        }
    }

    fn commit_purchase(
        &self,
        idem_key: &str,
        completed: &CompletedPurchase,
        inventory: &Inventory,
    ) -> Result<(), GameError> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| GameError::Storage("game state lock poisoned".into()))?;
        if let Some(PurchaseRecord::Completed(existing)) = state.purchases.get(idem_key) {
            if existing != completed {
                return Err(GameError::IdempotencyConflict);
            }
            return Ok(());
        }
        state
            .inventories
            .insert(completed.player.clone(), inventory.clone());
        state.purchases.insert(
            idem_key.into(),
            PurchaseRecord::Completed(completed.clone()),
        );
        Ok(())
    }

    fn save_inventory(&self, player: &str, inventory: &Inventory) -> Result<(), GameError> {
        self.inner
            .lock()
            .map_err(|_| GameError::Storage("game state lock poisoned".into()))?
            .inventories
            .insert(player.into(), inventory.clone());
        Ok(())
    }

    fn reserve_single_owner(
        &self,
        player: &str,
        ownership_key: &str,
        sku: &str,
        idem_key: &str,
    ) -> Result<(), GameError> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| GameError::Storage("game state lock poisoned".into()))?;
        let key = (player.to_string(), ownership_key.to_string());
        match state.reservations.get(&key) {
            Some(existing) if existing == idem_key => Ok(()),
            Some(_) => Err(GameError::AlreadyOwned(sku.to_string())),
            None => {
                state.reservations.insert(key, idem_key.to_string());
                Ok(())
            }
        }
    }

    fn release_single_owner(&self, player: &str, ownership_key: &str, idem_key: &str) {
        if let Ok(mut state) = self.inner.lock() {
            let key = (player.to_string(), ownership_key.to_string());
            if state.reservations.get(&key).map(String::as_str) == Some(idem_key) {
                state.reservations.remove(&key);
            }
        }
    }
}

/// Collect `player`'s completed purchases, newest settlement first. Shared by
/// every adapter so history ordering is identical across backends.
pub fn completed_for<'a>(
    records: impl Iterator<Item = &'a PurchaseRecord>,
    player: &str,
) -> Vec<CompletedPurchase> {
    let mut out: Vec<CompletedPurchase> = records
        .filter_map(|record| match record {
            PurchaseRecord::Completed(done) if done.player == player => Some(done.clone()),
            _ => None,
        })
        .collect();
    out.sort_by(|a, b| b.outcome.settlement_seq.cmp(&a.outcome.settlement_seq));
    out
}
