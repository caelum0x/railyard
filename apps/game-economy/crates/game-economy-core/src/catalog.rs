//! The typed game catalog: card packs (multiple price tiers), cosmetics/skins,
//! and a battle-pass. Every SKU carries its Mint price in microdollars, so the
//! whole storefront is priced on the same unit the ledger settles in.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::money::dollars;

/// Cosmetic rarity, ordered from most to least common. `Ord` lets us key the
/// per-rarity item pools in a deterministic `BTreeMap`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Rarity {
    /// The floor tier — highest drop weight.
    Common,
    /// A step up from common.
    Uncommon,
    /// Meaningfully scarce.
    Rare,
    /// Very scarce.
    Epic,
    /// The chase tier — lowest drop weight.
    Legendary,
}

impl Rarity {
    /// A short, stable tag used when content-addressing an item.
    pub fn tag(self) -> &'static str {
        match self {
            Rarity::Common => "common",
            Rarity::Uncommon => "uncommon",
            Rarity::Rare => "rare",
            Rarity::Epic => "epic",
            Rarity::Legendary => "legendary",
        }
    }
}

/// One card-pack price tier: how much it costs, how many cards it yields, and the
/// weighted rarity table those cards roll against.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardPackTier {
    /// Stable SKU, e.g. `pack.standard`.
    pub sku: String,
    /// Display name.
    pub name: String,
    /// Price in µ$.
    pub price_micros: i64,
    /// How many cards a single open yields.
    pub cards_per_pack: u32,
    /// Weighted rarity table (weight per rarity). Ordered for determinism.
    pub rarity_weights: Vec<(Rarity, u32)>,
}

/// A directly-purchasable cosmetic / skin. The `item_id` is content-addressed
/// (a blake3 of the item's identity) — the same scheme pack drops use, so a skin
/// pulled from a pack and a skin bought outright share one canonical id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cosmetic {
    /// Stable SKU, e.g. `skin.aeon.blade`.
    pub sku: String,
    /// Display name.
    pub name: String,
    /// Price in µ$.
    pub price_micros: i64,
    /// The equipment slot this cosmetic occupies.
    pub slot: String,
    /// Rarity of this skin — feeds its content-addressed id, so a skin bought
    /// outright and the same skin pulled from a pack share ONE canonical id.
    pub rarity: Rarity,
}

/// A battle-pass season SKU.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BattlePass {
    /// Stable SKU, e.g. `pass.season1`.
    pub sku: String,
    /// Display name.
    pub name: String,
    /// Price in µ$.
    pub price_micros: i64,
    /// Number of reward tiers in the pass.
    pub tiers: u32,
}

/// A purchasable product — the three game SKU shapes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Product {
    /// A card pack tier.
    CardPack(CardPackTier),
    /// A cosmetic / skin.
    Cosmetic(Cosmetic),
    /// A battle-pass.
    BattlePass(BattlePass),
}

impl Product {
    /// The product's stable SKU.
    pub fn sku(&self) -> &str {
        match self {
            Product::CardPack(p) => &p.sku,
            Product::Cosmetic(c) => &c.sku,
            Product::BattlePass(b) => &b.sku,
        }
    }

    /// The product's Mint price in µ$.
    pub fn price_micros(&self) -> i64 {
        match self {
            Product::CardPack(p) => p.price_micros,
            Product::Cosmetic(c) => c.price_micros,
            Product::BattlePass(b) => b.price_micros,
        }
    }
}

/// A read-only storefront: SKU -> product, plus the shared per-rarity item pools
/// that pack opening draws named items from.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    products: BTreeMap<String, Product>,
    pools: BTreeMap<Rarity, Vec<String>>,
}

impl Catalog {
    /// An empty catalog.
    pub fn empty() -> Self {
        Catalog::default()
    }

    /// Add or replace a product. Returns `self` for chaining.
    pub fn with_product(mut self, product: Product) -> Self {
        self.products.insert(product.sku().to_string(), product);
        self
    }

    /// Set the candidate item names for a rarity (the pool pack drops pick from).
    pub fn with_pool(mut self, rarity: Rarity, names: &[&str]) -> Self {
        self.pools
            .insert(rarity, names.iter().map(|s| s.to_string()).collect());
        self
    }

    /// Look up a product by SKU.
    pub fn get(&self, sku: &str) -> Option<&Product> {
        self.products.get(sku)
    }

    /// The candidate item names for a rarity (empty slice if unset).
    pub fn pool(&self, rarity: Rarity) -> &[String] {
        self.pools.get(&rarity).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Number of products in the catalog.
    pub fn len(&self) -> usize {
        self.products.len()
    }

    /// `true` if the catalog has no products.
    pub fn is_empty(&self) -> bool {
        self.products.is_empty()
    }

    /// The canonical demo storefront: three card-pack tiers, two skins, and a
    /// battle-pass, with a full per-rarity item pool. Prices are authored in
    /// dollars for readability but stored as µ$.
    pub fn demo() -> Self {
        Catalog::empty()
            // --- card packs: three price tiers, richer odds as price climbs ---
            .with_product(Product::CardPack(CardPackTier {
                sku: "pack.standard".into(),
                name: "Standard Pack".into(),
                price_micros: dollars(0, 99),
                cards_per_pack: 5,
                rarity_weights: vec![
                    (Rarity::Common, 60),
                    (Rarity::Uncommon, 25),
                    (Rarity::Rare, 10),
                    (Rarity::Epic, 4),
                    (Rarity::Legendary, 1),
                ],
            }))
            .with_product(Product::CardPack(CardPackTier {
                sku: "pack.premium".into(),
                name: "Premium Pack".into(),
                price_micros: dollars(2, 99),
                cards_per_pack: 5,
                rarity_weights: vec![
                    (Rarity::Common, 40),
                    (Rarity::Uncommon, 30),
                    (Rarity::Rare, 18),
                    (Rarity::Epic, 9),
                    (Rarity::Legendary, 3),
                ],
            }))
            .with_product(Product::CardPack(CardPackTier {
                sku: "pack.mega".into(),
                name: "Mega Pack".into(),
                price_micros: dollars(9, 99),
                cards_per_pack: 15,
                rarity_weights: vec![
                    (Rarity::Common, 35),
                    (Rarity::Uncommon, 30),
                    (Rarity::Rare, 20),
                    (Rarity::Epic, 10),
                    (Rarity::Legendary, 5),
                ],
            }))
            // --- cosmetics / skins ---
            .with_product(Product::Cosmetic(Cosmetic {
                sku: "skin.aeon.blade".into(),
                name: "Aeon Rift Blade".into(),
                price_micros: dollars(4, 99),
                slot: "weapon".into(),
                rarity: Rarity::Legendary,
            }))
            .with_product(Product::Cosmetic(Cosmetic {
                sku: "skin.rift.cloak".into(),
                name: "Riftwalker Cloak".into(),
                price_micros: dollars(7, 99),
                slot: "back".into(),
                rarity: Rarity::Epic,
            }))
            // --- battle-pass ---
            .with_product(Product::BattlePass(BattlePass {
                sku: "pass.season1".into(),
                name: "Season 1 Battle Pass".into(),
                price_micros: dollars(9, 99),
                tiers: 100,
            }))
            // --- per-rarity item pools pack drops draw from ---
            .with_pool(Rarity::Common, &["Iron Sigil", "Ash Banner", "Dust Charm"])
            .with_pool(Rarity::Uncommon, &["Copper Aegis", "Verdant Totem"])
            .with_pool(Rarity::Rare, &["Azure Relic", "Storm Brand"])
            .with_pool(Rarity::Epic, &["Void Crown", "Solar Halo"])
            .with_pool(Rarity::Legendary, &["Aeon Rift Blade"])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_catalog_has_all_three_sku_shapes() {
        let c = Catalog::demo();
        assert!(matches!(c.get("pack.standard"), Some(Product::CardPack(_))));
        assert!(matches!(
            c.get("skin.aeon.blade"),
            Some(Product::Cosmetic(_))
        ));
        assert!(matches!(
            c.get("pass.season1"),
            Some(Product::BattlePass(_))
        ));
        assert_eq!(c.get("nope"), None);
    }

    #[test]
    fn prices_are_in_micros() {
        let c = Catalog::demo();
        assert_eq!(c.get("pack.standard").unwrap().price_micros(), 990_000);
        assert_eq!(c.get("pack.mega").unwrap().price_micros(), 9_990_000);
    }

    #[test]
    fn pools_are_populated() {
        let c = Catalog::demo();
        assert_eq!(c.pool(Rarity::Legendary), &["Aeon Rift Blade".to_string()]);
        assert!(!c.pool(Rarity::Common).is_empty());
    }
}
