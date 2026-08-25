//! The item shop.
//!
//! Items are RON stat bundles (`content/items/*.ron`). Purchasing checks the team's
//! gold, deducts the cost, and applies the item's flat stat deltas to the buyer's
//! sim champion (bonus HP raises current + max, bonus AD/armor add on). Everything
//! is integer + deterministic.

use serde::{Deserialize, Serialize};
use sim::{EntityId, Health, World};

use super::map::{content_dir, load_ron, ContentError};

/// A purchasable item: a flat stat bundle at a gold cost.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Item {
    pub name: String,
    pub cost: i32,
    #[serde(default)]
    pub bonus_hp: i32,
    #[serde(default)]
    pub bonus_ad: i32,
    #[serde(default)]
    pub bonus_armor: i32,
}

/// Load a single item by file stem from `content/items/<stem>.ron`.
pub fn load_item(stem: &str) -> Result<Item, ContentError> {
    load_ron(content_dir().join("items").join(format!("{stem}.ron")))
}

/// Load every `content/items/*.ron` item, sorted by file name (deterministic order).
pub fn load_all_items() -> Result<Vec<Item>, ContentError> {
    let dir = content_dir().join("items");
    let mut paths: Vec<_> = std::fs::read_dir(&dir)
        .map_err(|e| ContentError::Io(format!("{}: {e}", dir.display())))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map(|x| x == "ron").unwrap_or(false))
        .collect();
    paths.sort();
    paths.into_iter().map(load_ron).collect()
}

/// Why a purchase failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShopError {
    NotEnoughGold,
    NoSuchBuyer,
    BuyerDead,
}

/// Buy `item` for the champion `buyer_id`, spending from `team_gold`. On success the
/// gold is deducted and the item's stats are applied to the buyer.
pub fn purchase(
    team_gold: &mut i32,
    world: &mut World,
    buyer_id: EntityId,
    item: &Item,
) -> Result<(), ShopError> {
    if *team_gold < item.cost {
        return Err(ShopError::NotEnoughGold);
    }
    let Some(c) = world.champion_mut(buyer_id) else {
        return Err(ShopError::NoSuchBuyer);
    };
    if !c.alive {
        return Err(ShopError::BuyerDead);
    }
    *team_gold -= item.cost;
    c.attack_damage += item.bonus_ad;
    c.armor += item.bonus_armor;
    if item.bonus_hp != 0 {
        c.health = Health {
            current: c.health.current + item.bonus_hp,
            max: c.health.max + item.bonus_hp,
        };
    }
    Ok(())
}
