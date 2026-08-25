//! (d) STAT-INHERITANCE gate — the reuse seam, proven.
//!
//! A card IS a champion. When a card is deployed, the resulting unit's combat stats
//! must come straight from the shared `ChampionSpec` (via `champions::compile`), with
//! zero re-authoring in the card layer. This test proves hp/ad/armor/range/attack-speed
//! flow from the roster spec into the live `sim::Champion`.

use cards::{elixir_cost, Card};
use champions::{compile, load_file, ChampionSpec};
use sim::{Vec2, TICK_HZ};

const SPECS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../content/champions");

fn spec(name: &str) -> ChampionSpec {
    load_file(format!("{SPECS_DIR}/{name}")).expect("roster spec must load + validate")
}

#[test]
fn card_unit_inherits_champion_stats_from_the_shared_spec() {
    let s = spec("sentinel.ron");
    // Ground-truth values authored in champions/specs/sentinel.ron.
    assert_eq!(s.base.hp, 620);
    assert_eq!(s.base.ad, 55);
    assert_eq!(s.base.armor, 30);
    assert_eq!(s.base.range, 8.0);
    assert_eq!(s.base.attack_speed, 1.0);

    let unit = Card::from_spec(s.clone()).spawn_unit(7, 0, Vec2::new(1.0, 2.0));

    // Stats came straight from the champion spec — not re-authored in the card layer.
    assert_eq!(unit.health.max, s.base.hp, "hp inherited");
    assert_eq!(unit.health.current, s.base.hp, "spawns at full hp");
    assert_eq!(unit.attack_damage, s.base.ad, "ad inherited");
    assert_eq!(unit.armor, s.base.armor, "armor inherited");
    assert_eq!(unit.attack_range, s.base.range, "range inherited");
    // attack_speed (attacks/sec) compiles to the sim's integer tick period.
    let expected_period = ((TICK_HZ as f32) / s.base.attack_speed).round().max(1.0) as u32;
    assert_eq!(unit.attack_period, expected_period, "attack period from attack_speed");
    assert_eq!(unit.id, 7);
    assert_eq!(unit.team, 0);
}

#[test]
fn spawn_unit_equals_the_champion_compiler_exactly() {
    // The reuse seam is literally `champions::compile`: a card's unit is byte-identical
    // to compiling the same spec directly. No divergent second implementation exists.
    for name in ["sentinel.ron", "pyromancer.ron", "vanguard.ron"] {
        let s = spec(name);
        let via_card = Card::from_spec(s.clone()).spawn_unit(3, 1, Vec2::new(-4.0, 5.0));
        let via_compiler = compile(&s, 3, 1, Vec2::new(-4.0, 5.0));
        assert_eq!(via_card, via_compiler, "card unit must equal compile() for {name}");
    }
}

#[test]
fn elixir_cost_is_deterministic_and_in_band() {
    // Costs are a pure integer function of champion power, in the 1..=10 band.
    for name in ["sentinel.ron", "pyromancer.ron", "vanguard.ron"] {
        let s = spec(name);
        let c = elixir_cost(&s);
        assert!((1..=10).contains(&c), "{name} cost {c} out of band");
        // Determinism: same spec => same cost, every call.
        assert_eq!(c, elixir_cost(&s));
    }
    // Concrete baselines from the power formula.
    assert_eq!(elixir_cost(&spec("sentinel.ron")), 4);
    assert_eq!(elixir_cost(&spec("pyromancer.ron")), 2);
    assert_eq!(elixir_cost(&spec("vanguard.ron")), 4);
}
