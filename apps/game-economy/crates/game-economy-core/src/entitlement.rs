//! What a player owns: entitlements + the per-player inventory that aggregates
//! them. Every entitlement is receipt-backed — it exists only because a Mint
//! capture settled — and carries a deterministic id so re-granting a replayed
//! purchase is a no-op (never a second copy).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::pack::GrantedItem;

/// A deterministic entitlement id (`ent_…`), derived from `(player, idem_key)`.
pub type EntitlementId = String;

/// An owned, unopened-or-opened card pack. The pack is grantable the instant its
/// purchase captures; its `contents` stay `None` until it is opened (which is a
/// pure function of `receipt_id` + a seed).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OwnedPack {
    /// This entitlement's id.
    pub entitlement_id: EntitlementId,
    /// The pack tier SKU that was bought.
    pub pack_sku: String,
    /// The Mint settlement sequence that paid for it.
    pub settlement_seq: u64,
    /// The content-addressed purchase receipt id the open() roll is anchored to.
    pub receipt_id: String,
    /// Materialized drops once opened; `None` while sealed.
    pub contents: Option<Vec<GrantedItem>>,
}

impl OwnedPack {
    /// `true` once the pack has been opened and its drops materialized.
    pub fn is_opened(&self) -> bool {
        self.contents.is_some()
    }
}

/// The player's battle-pass standing for a season.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BattlePassState {
    /// The pass SKU.
    pub pass_sku: String,
    /// Current unlocked tier (starts at 1 on purchase).
    pub tier: u32,
}

/// Everything a single player owns on the game rail.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Inventory {
    /// Owned packs, keyed by entitlement id.
    pub packs: BTreeMap<EntitlementId, OwnedPack>,
    /// Unlocked cosmetics: content-addressed `item_id` -> display name.
    pub cosmetics: BTreeMap<String, String>,
    /// Battle-pass standing, if any pass was bought.
    pub battle_pass: Option<BattlePassState>,
}

impl Inventory {
    /// A fresh, empty inventory.
    pub fn new() -> Self {
        Inventory::default()
    }

    /// Grant a pack. Idempotent: if the entitlement id is already present the
    /// existing pack (opened state and all) is left untouched.
    pub fn grant_pack(&mut self, pack: OwnedPack) {
        self.packs.entry(pack.entitlement_id.clone()).or_insert(pack);
    }

    /// Unlock a cosmetic by its content-addressed id. Idempotent.
    pub fn grant_cosmetic(&mut self, item_id: &str, name: &str) {
        self.cosmetics
            .entry(item_id.to_string())
            .or_insert_with(|| name.to_string());
    }

    /// Grant / (re)affirm a battle-pass at tier 1. Idempotent: an existing pass
    /// for the same SKU keeps its current tier.
    pub fn grant_battle_pass(&mut self, pass_sku: &str) {
        match &self.battle_pass {
            Some(bp) if bp.pass_sku == pass_sku => {}
            _ => {
                self.battle_pass = Some(BattlePassState {
                    pass_sku: pass_sku.to_string(),
                    tier: 1,
                });
            }
        }
    }

    /// `true` if the player already owns the given cosmetic id.
    pub fn owns_cosmetic(&self, item_id: &str) -> bool {
        self.cosmetics.contains_key(item_id)
    }

    /// `true` if the player already holds the given battle-pass SKU.
    pub fn owns_battle_pass(&self, pass_sku: &str) -> bool {
        self.battle_pass
            .as_ref()
            .map(|bp| bp.pass_sku == pass_sku)
            .unwrap_or(false)
    }
}
