//! (f) PACK-OPENING gate — deterministic, seeded card drops.
//!
//! Opening the same pack with the same seed always yields the same cards (the
//! franchise determinism law applied to the economy); distinct seeds generally differ;
//! and the pack's card count + guaranteed-rarity contract holds.

use cards::cards::pack::{load_pack, open_pack};
use cards::cards::CardPool;

fn pool() -> CardPool {
    CardPool::load_default().expect("card pool loads from content/champions")
}

#[test]
fn opening_a_pack_is_deterministic_per_seed() {
    let pool = pool();
    let pack = load_pack(cards::CONTENT_CARDS_DIR, "standard").expect("standard pack loads");

    let a = open_pack(&pool, &pack, 2024);
    let b = open_pack(&pool, &pack, 2024);

    assert_eq!(a.len(), pack.cards as usize, "pack yields its card count");
    assert_eq!(a, b, "same seed opens byte-identical cards");

    // Names match too (a human-readable determinism check).
    let names_a: Vec<&str> = a.iter().map(|c| c.name()).collect();
    let names_b: Vec<&str> = b.iter().map(|c| c.name()).collect();
    assert_eq!(names_a, names_b);
}

#[test]
fn distinct_seeds_generally_yield_distinct_packs() {
    let pool = pool();
    let pack = load_pack(cards::CONTENT_CARDS_DIR, "standard").expect("standard pack loads");

    let a: Vec<String> = open_pack(&pool, &pack, 1).iter().map(|c| c.name().to_string()).collect();
    let b: Vec<String> = open_pack(&pool, &pack, 424242).iter().map(|c| c.name().to_string()).collect();
    assert_ne!(a, b, "different seeds should produce different drops");
}

#[test]
fn guaranteed_tier_is_honored() {
    let pool = pool();
    let pack = load_pack(cards::CONTENT_CARDS_DIR, "standard").expect("standard pack loads");
    let table = pool.rarities();
    let (gi, _) = table
        .tier_by_name(pack.guaranteed_tier.as_deref().unwrap())
        .expect("guaranteed tier exists in the table");

    // Across many seeds, every pack contains at least one card of the guaranteed tier
    // or better (a higher tier index).
    for seed in 0..64u64 {
        let cards = open_pack(&pool, &pack, seed);
        let ok = cards.iter().any(|c| {
            table
                .tiers
                .iter()
                .position(|t| c.meta.cost >= t.min_cost && c.meta.cost <= t.max_cost)
                .map(|ti| ti >= gi)
                .unwrap_or(false)
        });
        assert!(ok, "seed {seed}: pack must guarantee a Rare-or-better card");
    }
}
