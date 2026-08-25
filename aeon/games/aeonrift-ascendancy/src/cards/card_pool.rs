//! The card pool + a player's collection.
//!
//! The pool is the whole roster turned into cards: every `content/champions/*.ron`
//! spec is loaded + validated by the shared `champions` DSL and wrapped in a
//! [`Card`] (which derives its elixir cost). Cards are keyed by their file **slug**
//! (e.g. `"ra-radiant-dawn"`) and kept in sorted slug order so the pool is
//! deterministic regardless of directory iteration order.
//!
//! [`Collection`] is a thin owned-card set for a player (used by the shop / pack
//! opener); the pool itself is the reference catalogue.

use std::path::Path;

use champions::load_file;

use super::card::Card;
use super::rarity::{RarityTable, RarityTier};
use super::CardError;

/// One catalogue entry: a champion's file slug plus its card form.
#[derive(Clone, Debug, PartialEq)]
pub struct PoolCard {
    pub slug: String,
    pub card: Card,
}

/// The full catalogue of cards (the roster) plus the rarity table used to classify
/// and draw them.
#[derive(Clone, Debug)]
pub struct CardPool {
    entries: Vec<PoolCard>,
    rarities: RarityTable,
}

impl CardPool {
    /// Build the pool by loading every `*.ron` in `champions_dir` (skipping the
    /// `roster_meta` sidecar), classified by `rarities`.
    pub fn from_dir(
        champions_dir: impl AsRef<Path>,
        rarities: RarityTable,
    ) -> Result<Self, CardError> {
        let dir = champions_dir.as_ref();
        let read = std::fs::read_dir(dir)
            .map_err(|e| CardError::Io(format!("{}: {e}", dir.display())))?;

        let mut entries: Vec<PoolCard> = Vec::new();
        for item in read {
            let item = item.map_err(|e| CardError::Io(e.to_string()))?;
            let path = item.path();
            if path.extension().and_then(|e| e.to_str()) != Some("ron") {
                continue;
            }
            let slug = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            // The sidecar is metadata, not a champion spec.
            if slug == "roster_meta" {
                continue;
            }
            let spec = load_file(&path)
                .map_err(|e| CardError::Content(format!("{slug}: {e}")))?;
            entries.push(PoolCard {
                slug,
                card: Card::from_spec(spec),
            });
        }

        // Deterministic order regardless of the filesystem's read_dir order.
        entries.sort_by(|a, b| a.slug.cmp(&b.slug));

        Ok(CardPool { entries, rarities })
    }

    /// Build the pool from the committed content directories (default paths).
    pub fn load_default() -> Result<Self, CardError> {
        Self::from_dir(crate::CONTENT_CHAMPIONS_DIR, super::rarity::default_rarity_table())
    }

    /// Number of cards in the pool.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// All catalogue entries in deterministic slug order.
    pub fn entries(&self) -> &[PoolCard] {
        &self.entries
    }

    /// The rarity table backing this pool.
    pub fn rarities(&self) -> &RarityTable {
        &self.rarities
    }

    /// Look up a card by its file slug.
    pub fn find_by_slug(&self, slug: &str) -> Option<&Card> {
        self.entries
            .iter()
            .find(|e| e.slug == slug)
            .map(|e| &e.card)
    }

    /// Look up a card by champion display name.
    pub fn find_by_name(&self, name: &str) -> Option<&Card> {
        self.entries
            .iter()
            .find(|e| e.card.name() == name)
            .map(|e| &e.card)
    }

    /// The rarity tier a card falls into (by its elixir cost).
    pub fn rarity_of(&self, card: &Card) -> Option<&RarityTier> {
        self.rarities.tier_for_cost(card.meta.cost)
    }

    /// Indices (into [`entries`](Self::entries)) of every card in the named tier, in
    /// deterministic order. Empty if the tier has no cards.
    pub fn indices_of_tier(&self, tier_name: &str) -> Vec<usize> {
        self.entries
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                self.rarities
                    .tier_for_cost(e.card.meta.cost)
                    .map(|t| t.name == tier_name)
                    .unwrap_or(false)
            })
            .map(|(i, _)| i)
            .collect()
    }
}

/// Load a single roster card by slug from a champions directory.
pub fn load_card_by_slug(
    champions_dir: impl AsRef<Path>,
    slug: &str,
) -> Result<Card, CardError> {
    let path = champions_dir.as_ref().join(format!("{slug}.ron"));
    let spec = load_file(&path).map_err(|e| CardError::Content(format!("{slug}: {e}")))?;
    Ok(Card::from_spec(spec))
}

/// A player's owned card collection (multiset of slugs). Used by the shop / packs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Collection {
    owned: Vec<String>,
}

impl Collection {
    /// An empty collection.
    pub fn new() -> Self {
        Collection::default()
    }

    /// Add an owned card slug.
    pub fn add(&mut self, slug: impl Into<String>) {
        self.owned.push(slug.into());
    }

    /// How many copies of `slug` are owned.
    pub fn count(&self, slug: &str) -> usize {
        self.owned.iter().filter(|s| s.as_str() == slug).count()
    }

    /// Does the collection own at least one copy of `slug`?
    pub fn owns(&self, slug: &str) -> bool {
        self.count(slug) > 0
    }

    /// Total cards owned (with duplicates).
    pub fn total(&self) -> usize {
        self.owned.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_loads_the_whole_roster_in_slug_order() {
        let pool = CardPool::load_default().expect("pool loads");
        assert!(pool.len() >= 40, "expected the full roster, got {}", pool.len());
        // Deterministic sorted order.
        let slugs: Vec<&str> = pool.entries().iter().map(|e| e.slug.as_str()).collect();
        let mut sorted = slugs.clone();
        sorted.sort_unstable();
        assert_eq!(slugs, sorted);
        // Known members resolve.
        assert!(pool.find_by_slug("sentinel").is_some());
        assert_eq!(pool.find_by_name("Pyromancer").is_some(), true);
    }

    #[test]
    fn collection_counts_copies() {
        let mut c = Collection::new();
        c.add("zeus");
        c.add("zeus");
        c.add("hades");
        assert_eq!(c.count("zeus"), 2);
        assert!(c.owns("hades"));
        assert_eq!(c.total(), 3);
    }
}
