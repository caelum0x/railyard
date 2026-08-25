//! Card rarities, loaded from `content/cards/rarities.ron`.
//!
//! A card's rarity is a pure function of its elixir cost: whichever tier's cost band
//! contains the cost. Tiers also carry a draw `weight` used by the deterministic pack
//! opener ([`super::pack`]). The canonical table is embedded at compile time (so the
//! default path needs no file IO and works on mobile), and can also be loaded from a
//! content directory for tooling.

use std::path::Path;

use serde::Deserialize;

use super::CardError;

/// One rarity tier: a name, a pack draw weight, and the inclusive elixir-cost band it
/// covers.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct RarityTier {
    pub name: String,
    pub weight: u32,
    pub min_cost: u32,
    pub max_cost: u32,
}

/// The ordered set of rarity tiers.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct RarityTable {
    pub tiers: Vec<RarityTier>,
}

/// The canonical rarity table, embedded from `content/cards/rarities.ron`.
const RARITIES_RON: &str =
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../content/cards/rarities.ron"));

impl RarityTable {
    /// The tier whose cost band contains `cost` (bands are expected contiguous).
    pub fn tier_for_cost(&self, cost: u32) -> Option<&RarityTier> {
        self.tiers
            .iter()
            .find(|t| cost >= t.min_cost && cost <= t.max_cost)
    }

    /// Find a tier (and its index) by name.
    pub fn tier_by_name(&self, name: &str) -> Option<(usize, &RarityTier)> {
        self.tiers
            .iter()
            .enumerate()
            .find(|(_, t)| t.name == name)
    }

    /// Total draw weight across all tiers (used to normalise pack rolls).
    pub fn total_weight(&self) -> u64 {
        self.tiers.iter().map(|t| t.weight as u64).sum()
    }
}

/// Parse a rarity table from RON text.
pub fn parse_rarity_table(ron_text: &str) -> Result<RarityTable, CardError> {
    ron::from_str::<RarityTable>(ron_text).map_err(|e| CardError::Parse(e.to_string()))
}

/// Load the rarity table from `<cards_dir>/rarities.ron`.
pub fn load_rarity_table(cards_dir: impl AsRef<Path>) -> Result<RarityTable, CardError> {
    let path = cards_dir.as_ref().join("rarities.ron");
    let text = std::fs::read_to_string(&path)
        .map_err(|e| CardError::Io(format!("{}: {e}", path.display())))?;
    parse_rarity_table(&text)
}

/// The compile-time-embedded canonical rarity table. Panics only if the committed
/// content file is malformed — a build invariant, not a runtime condition.
pub fn default_rarity_table() -> RarityTable {
    parse_rarity_table(RARITIES_RON).expect("embedded content/cards/rarities.ron must parse")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_table_has_contiguous_bands() {
        let t = default_rarity_table();
        assert!(!t.tiers.is_empty());
        assert_eq!(t.tier_for_cost(1).unwrap().name, "Common");
        assert_eq!(t.tier_for_cost(4).unwrap().name, "Rare");
        assert_eq!(t.tier_for_cost(10).unwrap().name, "Legendary");
        assert!(t.total_weight() > 0);
    }
}
