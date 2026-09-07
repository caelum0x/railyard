//! Deterministic, auditable pack opening.
//!
//! A pack's contents are a pure function of the **purchase receipt id** and a
//! **seed**: `contents = derive(tier, catalog, receipt_id, seed)`. Nothing else
//! feeds the roll, so anyone holding the receipt id + seed can recompute the exact
//! drops and verify a pack was opened honestly. The randomness comes from blake3
//! — the same hash Mint uses for its receipt chain and merkle proofs — so the
//! whole scheme is content-addressed end to end: each dropped item's id is the
//! hash of its identity, and each roll is the hash of `(receipt_id, seed, slot)`.

use serde::{Deserialize, Serialize};

use crate::catalog::{CardPackTier, Catalog, Rarity};

/// One item pulled from a pack (or bought outright).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantedItem {
    /// 0-based position within the opened pack.
    pub slot: u32,
    /// The rarity this slot rolled.
    pub rarity: Rarity,
    /// The item's display name (drawn from the catalog's per-rarity pool).
    pub name: String,
    /// Content-addressed item id (`item_…`): a blake3 of the item's identity.
    /// Identical items across packs/purchases collapse to one canonical id.
    pub item_id: String,
}

/// The content-addressed id for a cosmetic identity. Deterministic: same
/// `(rarity, name)` always yields the same `item_…` id, so a legendary pulled
/// from a pack and the same legendary sold as a skin are the SAME item.
pub fn content_item_id(rarity: Rarity, name: &str) -> String {
    let preimage = format!("cosmetic:v1:{}:{}", rarity.tag(), name);
    let hash = blake3::hash(preimage.as_bytes());
    format!("item_{}", &hash.to_hex()[..24])
}

/// Draw a `u64` from the roll domain: `blake3(receipt_id : seed : label)`, first
/// 8 bytes little-endian. Domain-separated by `label` so independent draws for
/// the same slot never correlate.
fn draw_u64(receipt_id: &str, seed: u64, label: &str) -> u64 {
    let preimage = format!("packroll:v1:{receipt_id}:{seed}:{label}");
    let hash = blake3::hash(preimage.as_bytes());
    let bytes = hash.as_bytes();
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[..8]);
    u64::from_le_bytes(buf)
}

/// Pick a rarity from the weighted table using `draw`. Falls back to the last
/// entry if the table's weights sum to zero (defensive; the demo never does).
fn pick_rarity(weights: &[(Rarity, u32)], draw: u64) -> Rarity {
    let total: u64 = weights.iter().map(|(_, w)| *w as u64).sum();
    if total == 0 {
        return weights.last().map(|(r, _)| *r).unwrap_or(Rarity::Common);
    }
    let mut pos = draw % total;
    for (rarity, weight) in weights {
        let w = *weight as u64;
        if pos < w {
            return *rarity;
        }
        pos -= w;
    }
    // Unreachable while `total > 0`, but stay total.
    weights.last().map(|(r, _)| *r).unwrap_or(Rarity::Common)
}

/// Derive a pack's full contents deterministically from the receipt id + seed.
///
/// For each slot: roll a rarity against the tier's weighted table, then pick a
/// named item from that rarity's catalog pool. Reproducible and auditable — the
/// same inputs always yield byte-identical output.
pub fn derive_pack_contents(
    tier: &CardPackTier,
    catalog: &Catalog,
    receipt_id: &str,
    seed: u64,
) -> Vec<GrantedItem> {
    let mut items = Vec::with_capacity(tier.cards_per_pack as usize);
    for slot in 0..tier.cards_per_pack {
        let rarity_draw = draw_u64(receipt_id, seed, &format!("slot:{slot}:rarity"));
        let rarity = pick_rarity(&tier.rarity_weights, rarity_draw);

        let pool = catalog.pool(rarity);
        let (name, item_id) = if pool.is_empty() {
            // No pool for this rarity: synthesize a stable placeholder identity so
            // the drop is still content-addressed and reproducible.
            let synth = format!("{}-{}", rarity.tag(), slot);
            let id = content_item_id(rarity, &synth);
            (synth, id)
        } else {
            let item_draw = draw_u64(receipt_id, seed, &format!("slot:{slot}:item"));
            let idx = (item_draw % pool.len() as u64) as usize;
            let name = pool[idx].clone();
            let id = content_item_id(rarity, &name);
            (name, id)
        };

        items.push(GrantedItem {
            slot,
            rarity,
            name,
            item_id,
        });
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tier() -> CardPackTier {
        match Catalog::demo().get("pack.standard").unwrap().clone() {
            crate::catalog::Product::CardPack(t) => t,
            _ => unreachable!(),
        }
    }

    #[test]
    fn content_id_is_stable_and_collapses_identical_items() {
        let a = content_item_id(Rarity::Legendary, "Aeon Rift Blade");
        let b = content_item_id(Rarity::Legendary, "Aeon Rift Blade");
        assert_eq!(a, b);
        assert!(a.starts_with("item_"));
        // Different identity -> different id.
        assert_ne!(a, content_item_id(Rarity::Common, "Aeon Rift Blade"));
    }

    #[test]
    fn opening_is_deterministic_per_receipt_and_seed() {
        let c = Catalog::demo();
        let t = tier();
        let a = derive_pack_contents(&t, &c, "rcpt_abc", 42);
        let b = derive_pack_contents(&t, &c, "rcpt_abc", 42);
        assert_eq!(a, b);
        assert_eq!(a.len(), t.cards_per_pack as usize);
    }

    #[test]
    fn different_receipt_or_seed_changes_the_roll() {
        let c = Catalog::demo();
        let t = tier();
        let base = derive_pack_contents(&t, &c, "rcpt_abc", 42);
        let other_receipt = derive_pack_contents(&t, &c, "rcpt_xyz", 42);
        let other_seed = derive_pack_contents(&t, &c, "rcpt_abc", 43);
        assert_ne!(base, other_receipt);
        assert_ne!(base, other_seed);
    }
}
