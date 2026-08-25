//! (h) POOL STAT-INHERITANCE gate — cards built from the roster keep champion stats.
//!
//! Cards loaded through the card POOL (from `content/champions`) must, when deployed,
//! produce units whose combat stats are exactly those the shared `champions` compiler
//! yields — proving the pool loader introduces no divergent stat path.

use cards::cards::CardPool;
use champions::{compile, load_file};
use sim::Vec2;

#[test]
fn pool_cards_inherit_champion_stats_via_the_shared_compiler() {
    let pool = CardPool::load_default().expect("pool loads from content/champions");

    for slug in ["sentinel", "pyromancer", "vanguard", "zeus", "ra-radiant-dawn"] {
        let card = pool
            .find_by_slug(slug)
            .unwrap_or_else(|| panic!("{slug} in pool"));

        // The unit a pool card spawns must equal compiling the same spec directly.
        let spec = load_file(format!("{}/{slug}.ron", cards::CONTENT_CHAMPIONS_DIR))
            .expect("spec loads");
        let via_card = card.spawn_unit(9, 0, Vec2::new(2.0, -3.0));
        let via_compiler = compile(&spec, 9, 0, Vec2::new(2.0, -3.0));
        assert_eq!(via_card, via_compiler, "{slug}: pool card unit must match compile()");

        // And the card's core stats came straight from the champion spec.
        assert_eq!(via_card.health.max, spec.base.hp, "{slug} hp inherited");
        assert_eq!(via_card.attack_damage, spec.base.ad, "{slug} ad inherited");
        assert_eq!(via_card.armor, spec.base.armor, "{slug} armor inherited");
    }
}

#[test]
fn pool_classifies_every_card_into_a_rarity_tier() {
    let pool = CardPool::load_default().expect("pool loads");
    for entry in pool.entries() {
        assert!(
            pool.rarity_of(&entry.card).is_some(),
            "{}: cost {} must fall in a rarity band",
            entry.slug,
            entry.card.meta.cost
        );
    }
}
