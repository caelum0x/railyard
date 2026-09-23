//! Atomic, restart-safe local state for the game storefront.
//!
//! This adapter is intended for one game service/desktop process. Every mutation
//! rewrites a complete JSON snapshot to a sibling temporary file, syncs it, and
//! atomically renames it over the live snapshot. Horizontal deployments should
//! implement the same `GameStateRepository` port with a transactional database.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use game_economy_core::{
    CompletedPurchase, GameError, GameStateRepository, Inventory, PurchaseRecord,
};
use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
struct Snapshot {
    inventories: HashMap<String, Inventory>,
    purchases: HashMap<String, PurchaseRecord>,
    /// In-flight single-owner reservations: `(player, ownership_key) -> idem_key`.
    /// Deliberately NOT persisted (`serde(skip)`): a reservation only guards
    /// concurrent in-flight purchases within one live process. On restart there
    /// are no in-flight purchases, and the persisted inventory + pre-hold
    /// ownership guard cover the already-granted case. Held under the same mutex
    /// as `inventories`, so check-and-reserve is atomic.
    #[serde(skip)]
    reservations: HashMap<(String, String), String>,
}

pub struct JsonFileGameState {
    path: PathBuf,
    state: Mutex<Snapshot>,
}

impl JsonFileGameState {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, GameError> {
        let path = path.into();
        let state = match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map_err(|e| GameError::Storage(format!("decode {}: {e}", path.display())))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Snapshot::default(),
            Err(error) => {
                return Err(GameError::Storage(format!(
                    "read {}: {error}",
                    path.display()
                )))
            }
        };
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    fn persist(&self, state: &Snapshot) -> Result<(), GameError> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)
                .map_err(|e| GameError::Storage(format!("create {}: {e}", parent.display())))?;
        }
        let temp = temp_path(&self.path);
        let bytes = serde_json::to_vec(state)
            .map_err(|e| GameError::Storage(format!("encode game state: {e}")))?;
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .open(&temp)
            .map_err(|e| GameError::Storage(format!("open {}: {e}", temp.display())))?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|e| GameError::Storage(format!("write {}: {e}", temp.display())))?;
        fs::rename(&temp, &self.path).map_err(|e| {
            GameError::Storage(format!(
                "replace {} from {}: {e}",
                self.path.display(),
                temp.display()
            ))
        })?;
        sync_parent(&self.path)?;
        Ok(())
    }
}

impl GameStateRepository for JsonFileGameState {
    fn inventory(&self, player: &str) -> Result<Inventory, GameError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| GameError::Storage("game state lock poisoned".into()))?
            .inventories
            .get(player)
            .cloned()
            .unwrap_or_default())
    }

    fn purchase(&self, idem_key: &str) -> Result<Option<PurchaseRecord>, GameError> {
        Ok(self
            .state
            .lock()
            .map_err(|_| GameError::Storage("game state lock poisoned".into()))?
            .purchases
            .get(idem_key)
            .cloned())
    }

    fn purchases_for(&self, player: &str) -> Result<Vec<CompletedPurchase>, GameError> {
        let guard = self
            .state
            .lock()
            .map_err(|_| GameError::Storage("game state lock poisoned".into()))?;
        Ok(game_economy_core::completed_for(guard.purchases.values(), player))
    }

    fn begin_purchase(
        &self,
        idem_key: &str,
        player: &str,
        product_sku: &str,
    ) -> Result<(), GameError> {
        let mut state = self
            .state
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
                self.persist(&state)
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
            .state
            .lock()
            .map_err(|_| GameError::Storage("game state lock poisoned".into()))?;
        if let Some(PurchaseRecord::Completed(existing)) = state.purchases.get(idem_key) {
            return if existing == completed {
                Ok(())
            } else {
                Err(GameError::IdempotencyConflict)
            };
        }
        state
            .inventories
            .insert(completed.player.clone(), inventory.clone());
        state.purchases.insert(
            idem_key.into(),
            PurchaseRecord::Completed(completed.clone()),
        );
        self.persist(&state)
    }

    fn save_inventory(&self, player: &str, inventory: &Inventory) -> Result<(), GameError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| GameError::Storage("game state lock poisoned".into()))?;
        state.inventories.insert(player.into(), inventory.clone());
        self.persist(&state)
    }

    fn reserve_single_owner(
        &self,
        player: &str,
        ownership_key: &str,
        sku: &str,
        idem_key: &str,
    ) -> Result<(), GameError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| GameError::Storage("game state lock poisoned".into()))?;
        let key = (player.to_string(), ownership_key.to_string());
        match state.reservations.get(&key) {
            Some(existing) if existing == idem_key => Ok(()),
            Some(_) => Err(GameError::AlreadyOwned(sku.to_string())),
            None => {
                // Purely in-memory concurrency guard — no snapshot rewrite.
                state.reservations.insert(key, idem_key.to_string());
                Ok(())
            }
        }
    }

    fn release_single_owner(&self, player: &str, ownership_key: &str, idem_key: &str) {
        if let Ok(mut state) = self.state.lock() {
            let key = (player.to_string(), ownership_key.to_string());
            if state.reservations.get(&key).map(String::as_str) == Some(idem_key) {
                state.reservations.remove(&key);
            }
        }
    }
}

fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("game-state.json");
    path.with_file_name(format!(".{name}.tmp"))
}

fn sync_parent(path: &Path) -> Result<(), GameError> {
    let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return Ok(());
    };
    File::open(parent)
        .and_then(|dir| dir.sync_all())
        .map_err(|e| GameError::Storage(format!("sync {}: {e}", parent.display())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use game_economy_core::{PurchaseOutcome, SettlementProof};

    #[test]
    fn committed_entitlement_survives_reopen() {
        let path = std::env::temp_dir().join(format!(
            "game-economy-state-{}-{}.json",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let _ = fs::remove_file(&path);
        let state = JsonFileGameState::open(&path).unwrap();
        state
            .begin_purchase("buy-1", "acct_player", "pack.standard")
            .unwrap();
        let outcome = PurchaseOutcome {
            entitlement_id: "ent_1".into(),
            product_sku: "pack.standard".into(),
            charged_micros: 990_000,
            receipt_id: "hash_1".into(),
            settlement_seq: 7,
            settlement_proof: SettlementProof {
                sequence: 7,
                operation: "hold.capture".into(),
                operation_hash: "op".into(),
                receipt_hash: "hash_1".into(),
                signature: "sig".into(),
            },
            idempotent_replay: false,
        };
        let completed = CompletedPurchase {
            player: "acct_player".into(),
            outcome,
        };
        let mut inventory = Inventory::default();
        inventory.grant_cosmetic("skin_1", "Blade");
        state
            .commit_purchase("buy-1", &completed, &inventory)
            .unwrap();
        drop(state);

        let reopened = JsonFileGameState::open(&path).unwrap();
        assert_eq!(
            reopened.purchase("buy-1").unwrap(),
            Some(PurchaseRecord::Completed(completed))
        );
        assert!(reopened
            .inventory("acct_player")
            .unwrap()
            .owns_cosmetic("skin_1"));
        let _ = fs::remove_file(path);
    }
}
