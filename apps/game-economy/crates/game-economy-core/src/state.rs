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
    /// Set once the purchase has been refunded (money returned through Mint) or
    /// revoked (money reversed outside the store, e.g. a card chargeback). A
    /// purchase can be reversed at most once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reversal: Option<PurchaseReversal>,
}

impl CompletedPurchase {
    /// A freshly settled purchase (not reversed).
    pub fn new(player: impl Into<String>, outcome: PurchaseOutcome) -> Self {
        Self {
            player: player.into(),
            outcome,
            reversal: None,
        }
    }

    /// `completed`, `refunded` or `revoked` — the status the history APIs show.
    pub fn status(&self) -> &'static str {
        match &self.reversal {
            None => "completed",
            Some(r) => r.kind.as_str(),
        }
    }

    /// Same settled purchase, ignoring the reversal and replay flag. Used by
    /// adapters to decide whether a re-commit is an idempotent replay.
    pub fn same_settlement(&self, other: &CompletedPurchase) -> bool {
        self.player == other.player
            && self.outcome.receipt_id == other.outcome.receipt_id
            && self.outcome.entitlement_id == other.outcome.entitlement_id
            && self.outcome.product_sku == other.outcome.product_sku
            && self.outcome.charged_micros == other.outcome.charged_micros
    }
}

/// How a purchase was reversed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReversalKind {
    /// Money returned to the player by a Mint transfer from the revenue account.
    Refunded,
    /// Entitlement removed; money reversed outside the store (chargeback).
    Revoked,
}

impl ReversalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ReversalKind::Refunded => "refunded",
            ReversalKind::Revoked => "revoked",
        }
    }
}

/// The durable record of a refund or revocation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurchaseReversal {
    pub kind: ReversalKind,
    /// Unix seconds the reversal was committed.
    pub at_unix: u64,
    /// Amount returned to the player through Mint (0 for a revocation).
    pub refunded_micros: i64,
    /// Mint ledger sequence of the refund transfer (refunds only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mint_seq: Option<u64>,
    /// Mint receipt hash of the refund transfer (refunds only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mint_receipt: Option<String>,
    /// Operator / game-backend supplied reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Result of [`GameStateRepository::reverse_purchase`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReverseOutcome {
    /// No completed purchase with that receipt for the player.
    NotFound,
    /// The purchase had already been reversed; nothing changed.
    AlreadyReversed(CompletedPurchase),
    /// Reversal recorded and the entitlement removed, atomically.
    Reversed(CompletedPurchase),
}

/// Store-wide totals for the operator console.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurchaseSummary {
    pub completed_purchases: u64,
    pub pending_purchases: u64,
    pub players: u64,
    pub gross_micros: i64,
    pub refunded_count: u64,
    pub refunded_micros: i64,
    pub revoked_count: u64,
    /// Gross minus money returned through refunds.
    pub net_micros: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PurchaseRecord {
    Pending { player: String, product_sku: String },
    Completed(CompletedPurchase),
}

/// Who holds a single-owner reservation, and for how long.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReservationHolder {
    pub idem_key: String,
    pub age_ms: i64,
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
    /// The memory/JSON adapters keep reservations process-local (single
    /// instance only). The Postgres adapter (`game-economy-store-postgres`)
    /// backs them with a `(player, ownership_key)` primary key, so the guard
    /// holds across horizontally-scaled instances. Reservations are cleared on
    /// [`GameStateRepository::release_single_owner`] (failure path) and when the
    /// purchase is reversed ([`GameStateRepository::reverse_purchase`]).
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

    /// The current holder of a single-owner reservation, if any.
    fn single_owner_holder(
        &self,
        player: &str,
        ownership_key: &str,
    ) -> Result<Option<ReservationHolder>, GameError>;

    /// Move a reservation from `from_idem` to `to_idem` (restarting its age),
    /// only if `from_idem` still holds it. Used after the store proved the old
    /// holder's purchase can never charge (see `GameStore::purchase`).
    fn take_over_single_owner(
        &self,
        player: &str,
        ownership_key: &str,
        from_idem: &str,
        to_idem: &str,
    ) -> Result<bool, GameError>;

    /// Atomic read-modify-write of one player's inventory. Transactional
    /// adapters (Postgres) row-lock the player for the duration of `apply`, so
    /// concurrent pack opens / pass advances on DIFFERENT server instances
    /// serialize. The default is read → apply → save, which is only safe when
    /// the caller serializes (the in-process `GameStore` lock does).
    fn update_inventory(
        &self,
        player: &str,
        apply: &mut dyn FnMut(&mut Inventory) -> Result<(), GameError>,
    ) -> Result<Inventory, GameError> {
        let mut inventory = self.inventory(player)?;
        apply(&mut inventory)?;
        self.save_inventory(player, &inventory)?;
        Ok(inventory)
    }

    /// Commit a settled purchase, applying `grant` to the player's CURRENT
    /// inventory inside the same atomic unit (no lost update when another
    /// instance granted to the same player concurrently).
    fn commit_purchase_with(
        &self,
        idem_key: &str,
        completed: &CompletedPurchase,
        grant: &dyn Fn(&mut Inventory),
    ) -> Result<(), GameError> {
        let mut inventory = self.inventory(&completed.player)?;
        grant(&mut inventory);
        self.commit_purchase(idem_key, completed, &inventory)
    }

    /// Atomically mark `player`'s purchase `receipt_id` reversed, remove what it
    /// granted (`revoke`), and free its single-owner reservation
    /// (`ownership_key`) so the good can be bought again. A purchase is reversed
    /// at most once: a second call returns [`ReverseOutcome::AlreadyReversed`].
    fn reverse_purchase(
        &self,
        player: &str,
        receipt_id: &str,
        reversal: &PurchaseReversal,
        ownership_key: Option<&str>,
        revoke: &dyn Fn(&CompletedPurchase, &mut Inventory),
    ) -> Result<ReverseOutcome, GameError>;

    /// The most recent completed purchases across ALL players, newest first
    /// (operator console).
    fn recent_purchases(&self, limit: usize) -> Result<Vec<CompletedPurchase>, GameError>;

    /// Store-wide totals (operator console).
    fn summary(&self) -> Result<PurchaseSummary, GameError>;
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
    /// When each reservation was taken.
    reserved_at: HashMap<(String, String), std::time::Instant>,
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
            if !existing.same_settlement(completed) {
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
                state.reserved_at.insert(key.clone(), std::time::Instant::now());
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
                state.reserved_at.remove(&key);
            }
        }
    }

    fn single_owner_holder(
        &self,
        player: &str,
        ownership_key: &str,
    ) -> Result<Option<ReservationHolder>, GameError> {
        let state = self
            .inner
            .lock()
            .map_err(|_| GameError::Storage("game state lock poisoned".into()))?;
        let key = (player.to_string(), ownership_key.to_string());
        Ok(state.reservations.get(&key).map(|idem_key| ReservationHolder {
            idem_key: idem_key.clone(),
            age_ms: state
                .reserved_at
                .get(&key)
                .map(|t| i64::try_from(t.elapsed().as_millis()).unwrap_or(i64::MAX))
                .unwrap_or(i64::MAX),
        }))
    }

    fn take_over_single_owner(
        &self,
        player: &str,
        ownership_key: &str,
        from_idem: &str,
        to_idem: &str,
    ) -> Result<bool, GameError> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| GameError::Storage("game state lock poisoned".into()))?;
        let key = (player.to_string(), ownership_key.to_string());
        if state.reservations.get(&key).map(String::as_str) != Some(from_idem) {
            return Ok(false);
        }
        state.reservations.insert(key.clone(), to_idem.to_string());
        state.reserved_at.insert(key, std::time::Instant::now());
        Ok(true)
    }

    fn reverse_purchase(
        &self,
        player: &str,
        receipt_id: &str,
        reversal: &PurchaseReversal,
        ownership_key: Option<&str>,
        revoke: &dyn Fn(&CompletedPurchase, &mut Inventory),
    ) -> Result<ReverseOutcome, GameError> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| GameError::Storage("game state lock poisoned".into()))?;
        let snapshot = &mut *state;
        let outcome = reverse_in(
            &mut snapshot.purchases,
            &mut snapshot.inventories,
            player,
            receipt_id,
            reversal,
            revoke,
        );
        if let (ReverseOutcome::Reversed(_), Some(key)) = (&outcome, ownership_key) {
            snapshot
                .reservations
                .remove(&(player.to_string(), key.to_string()));
        }
        Ok(outcome)
    }

    fn recent_purchases(&self, limit: usize) -> Result<Vec<CompletedPurchase>, GameError> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| GameError::Storage("game state lock poisoned".into()))?;
        Ok(recent_of(guard.purchases.values(), limit))
    }

    fn summary(&self) -> Result<PurchaseSummary, GameError> {
        let guard = self
            .inner
            .lock()
            .map_err(|_| GameError::Storage("game state lock poisoned".into()))?;
        Ok(summarize(guard.purchases.values()))
    }
}

/// Shared reversal logic for the map-backed adapters (memory + JSON file):
/// find the completed purchase by `(player, receipt_id)`, refuse a second
/// reversal, apply `revoke` to the inventory and stamp the reversal.
pub fn reverse_in(
    purchases: &mut HashMap<String, PurchaseRecord>,
    inventories: &mut HashMap<String, Inventory>,
    player: &str,
    receipt_id: &str,
    reversal: &PurchaseReversal,
    revoke: &dyn Fn(&CompletedPurchase, &mut Inventory),
) -> ReverseOutcome {
    let Some(record) = purchases.values_mut().find(|r| {
        matches!(r, PurchaseRecord::Completed(done)
            if done.player == player && done.outcome.receipt_id == receipt_id)
    }) else {
        return ReverseOutcome::NotFound;
    };
    let PurchaseRecord::Completed(done) = record else {
        return ReverseOutcome::NotFound;
    };
    if done.reversal.is_some() {
        return ReverseOutcome::AlreadyReversed(done.clone());
    }
    let inventory = inventories.entry(player.to_string()).or_default();
    revoke(done, inventory);
    done.reversal = Some(reversal.clone());
    ReverseOutcome::Reversed(done.clone())
}

/// The newest `limit` completed purchases across all players.
pub fn recent_of<'a>(
    records: impl Iterator<Item = &'a PurchaseRecord>,
    limit: usize,
) -> Vec<CompletedPurchase> {
    let mut out: Vec<CompletedPurchase> = records
        .filter_map(|record| match record {
            PurchaseRecord::Completed(done) => Some(done.clone()),
            PurchaseRecord::Pending { .. } => None,
        })
        .collect();
    out.sort_by(|a, b| b.outcome.settlement_seq.cmp(&a.outcome.settlement_seq));
    out.truncate(limit);
    out
}

/// Aggregate store-wide totals from purchase records.
pub fn summarize<'a>(records: impl Iterator<Item = &'a PurchaseRecord>) -> PurchaseSummary {
    let mut summary = PurchaseSummary::default();
    let mut players = std::collections::HashSet::new();
    for record in records {
        match record {
            PurchaseRecord::Pending { .. } => summary.pending_purchases += 1,
            PurchaseRecord::Completed(done) => {
                summary.completed_purchases += 1;
                summary.gross_micros += done.outcome.charged_micros;
                players.insert(done.player.as_str());
                match done.reversal.as_ref().map(|r| (r.kind, r.refunded_micros)) {
                    Some((ReversalKind::Refunded, micros)) => {
                        summary.refunded_count += 1;
                        summary.refunded_micros += micros;
                    }
                    Some((ReversalKind::Revoked, _)) => summary.revoked_count += 1,
                    None => {}
                }
            }
        }
    }
    summary.players = players.len() as u64;
    summary.net_micros = summary.gross_micros - summary.refunded_micros;
    summary
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
