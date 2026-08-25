//! Pack opening — deterministic, seeded card drops.
//!
//! A [`PackDef`] (loaded from `content/cards/packs/*.ron`) says how many cards a pack
//! yields and the minimum rarity it guarantees. [`open_pack`] rolls each slot's rarity
//! by tier weight, then picks a card of that rarity from the pool — all driven by a
//! seeded [`sim::Rng`], so opening the same pack with the same seed always yields the
//! same cards (the franchise determinism law, applied to the economy).

use std::path::Path;

use serde::Deserialize;
use sim::Rng;

use super::card::Card;
use super::card_pool::CardPool;
use super::rarity::RarityTable;
use super::CardError;

/// A pack definition: how many cards it drops and the minimum rarity guaranteed.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct PackDef {
    pub name: String,
    pub cards: u32,
    /// If set, at least one card in the pack is of this tier or better.
    #[serde(default)]
    pub guaranteed_tier: Option<String>,
}

/// Parse a pack definition from RON text.
pub fn parse_pack(ron_text: &str) -> Result<PackDef, CardError> {
    ron::from_str::<PackDef>(ron_text).map_err(|e| CardError::Parse(e.to_string()))
}

/// Load a pack definition from `<cards_dir>/packs/<name>.ron`.
pub fn load_pack(cards_dir: impl AsRef<Path>, name: &str) -> Result<PackDef, CardError> {
    let path = cards_dir.as_ref().join("packs").join(format!("{name}.ron"));
    let text = std::fs::read_to_string(&path)
        .map_err(|e| CardError::Io(format!("{}: {e}", path.display())))?;
    parse_pack(&text)
}

/// The tier index (0 = most common) a card's cost maps to, if any.
fn tier_index_of_cost(table: &RarityTable, cost: u32) -> Option<usize> {
    table
        .tiers
        .iter()
        .position(|t| cost >= t.min_cost && cost <= t.max_cost)
}

/// Roll a tier index by weight from the seeded RNG.
fn roll_tier(table: &RarityTable, rng: &mut Rng, total: u64) -> usize {
    let mut r = rng.next_u64() % total;
    for (i, t) in table.tiers.iter().enumerate() {
        let w = t.weight as u64;
        if r < w {
            return i;
        }
        r -= w;
    }
    table.tiers.len().saturating_sub(1)
}

/// Pick a pool index for a card of the given tier; fall back to the whole pool if the
/// tier happens to have no cards.
fn pick_from_tier(pool: &CardPool, tier_index: usize, rng: &mut Rng) -> usize {
    let tier_name = &pool.rarities().tiers[tier_index].name;
    let candidates = pool.indices_of_tier(tier_name);
    if !candidates.is_empty() {
        let k = (rng.next_u64() % candidates.len() as u64) as usize;
        candidates[k]
    } else {
        // Degenerate: no card in this tier — draw from the whole pool instead.
        (rng.next_u64() % pool.len().max(1) as u64) as usize
    }
}

/// Open a pack deterministically from `seed`, returning the dropped cards in draw
/// order. Honors the pack's `guaranteed_tier` (at least one card of that tier or
/// better) by upgrading the final slot if the natural rolls missed it.
pub fn open_pack(pool: &CardPool, pack: &PackDef, seed: u64) -> Vec<Card> {
    let table = pool.rarities();
    let total = table.total_weight().max(1);
    let mut rng = Rng::new(seed);

    let mut picks: Vec<usize> = Vec::with_capacity(pack.cards as usize);
    for _ in 0..pack.cards {
        let tier = roll_tier(table, &mut rng, total);
        picks.push(pick_from_tier(pool, tier, &mut rng));
    }

    if let Some(guaranteed) = &pack.guaranteed_tier {
        if let Some((gi, _)) = table.tier_by_name(guaranteed) {
            let satisfied = picks.iter().any(|&i| {
                tier_index_of_cost(table, pool.entries()[i].card.meta.cost)
                    .map(|ti| ti >= gi)
                    .unwrap_or(false)
            });
            if !satisfied && !picks.is_empty() {
                let upgraded = pick_from_tier(pool, gi, &mut rng);
                *picks.last_mut().unwrap() = upgraded;
            }
        }
    }

    picks
        .into_iter()
        .map(|i| pool.entries()[i].card.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> CardPool {
        CardPool::load_default().expect("pool loads")
    }

    #[test]
    fn standard_pack_parses() {
        let pack = load_pack(crate::CONTENT_CARDS_DIR, "standard").expect("pack loads");
        assert_eq!(pack.cards, 5);
        assert_eq!(pack.guaranteed_tier.as_deref(), Some("Rare"));
    }

    #[test]
    fn opening_is_deterministic_per_seed() {
        let pool = pool();
        let pack = load_pack(crate::CONTENT_CARDS_DIR, "standard").expect("pack loads");
        let a = open_pack(&pool, &pack, 12345);
        let b = open_pack(&pool, &pack, 12345);
        assert_eq!(a.len(), pack.cards as usize);
        assert_eq!(a, b, "same seed must open the same cards");
    }

    #[test]
    fn different_seed_generally_differs() {
        let pool = pool();
        let pack = load_pack(crate::CONTENT_CARDS_DIR, "standard").expect("pack loads");
        let a: Vec<String> = open_pack(&pool, &pack, 1).iter().map(|c| c.name().to_string()).collect();
        let b: Vec<String> = open_pack(&pool, &pack, 999).iter().map(|c| c.name().to_string()).collect();
        assert_ne!(a, b, "distinct seeds should usually differ");
    }
}
