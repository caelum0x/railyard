//! (a) DETERMINISM gate — the same law as the MOBA sim.
//!
//! Same seed + same scripted deploys => byte-identical final MatchState AND identical
//! card-game state hash, run twice. Also: a bot-vs-bot match is reproducible.

use cards::{
    match_hash, run_bot_match, run_scripted_match, Card, Deploy, MatchConfig,
    DEFAULT_DEPLOY_INTERVAL,
};
use champions::{load_file, ChampionSpec};

const SPECS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../content/champions");

fn spec(name: &str) -> ChampionSpec {
    load_file(format!("{SPECS_DIR}/{name}")).expect("roster spec must load + validate")
}

fn card(name: &str) -> Card {
    Card::from_spec(spec(name))
}

/// A fixed scripted schedule that deploys units for both players across several ticks
/// and into several lanes, so the whole pipeline (deploy, march, fight, death) runs.
fn schedule() -> Vec<(u64, Deploy)> {
    vec![
        (10, Deploy::new(0, 0, 0)),
        (10, Deploy::new(1, 0, 0)),
        (60, Deploy::new(0, 1, 1)),
        (60, Deploy::new(1, 0, 1)),
        (120, Deploy::new(0, 0, 2)),
        (120, Deploy::new(1, 0, 2)),
        (200, Deploy::new(0, 1, 0)),
    ]
}

#[test]
fn scripted_match_is_deterministic_twice() {
    let cfg = MatchConfig::default();
    let sched = schedule();

    let a = run_scripted_match(cfg, vec![card("sentinel.ron"), card("vanguard.ron")], vec![card("pyromancer.ron")], &sched);
    let b = run_scripted_match(cfg, vec![card("sentinel.ron"), card("vanguard.ron")], vec![card("pyromancer.ron")], &sched);

    // Full-state equality (every field), not just the hash.
    assert_eq!(a, b, "scripted match diverged across two identical runs");
    // And the card-game state hash matches too.
    assert_eq!(match_hash(&a), match_hash(&b), "match hash diverged");
    // The scripted run must actually have progressed (sanity: not a no-op).
    assert!(a.world.tick > 0, "match did not advance");
    assert!(
        a.deployed_count[0] + a.deployed_count[1] > 0,
        "no units were deployed"
    );
}

#[test]
fn bot_match_is_deterministic_twice() {
    let cfg = MatchConfig::default();

    let a = run_bot_match(
        cfg,
        vec![card("sentinel.ron"), card("vanguard.ron")],
        vec![card("pyromancer.ron")],
        DEFAULT_DEPLOY_INTERVAL,
    );
    let b = run_bot_match(
        cfg,
        vec![card("sentinel.ron"), card("vanguard.ron")],
        vec![card("pyromancer.ron")],
        DEFAULT_DEPLOY_INTERVAL,
    );

    assert_eq!(a, b, "bot match diverged across two identical runs");
    assert_eq!(match_hash(&a), match_hash(&b), "match hash diverged");
}

#[test]
fn different_seed_changes_nothing_for_rng_free_combat_but_hash_is_seeded() {
    // Combat here is RNG-free, but the seed is part of the world state + hash (same
    // faithful contract as the sim duel harness): a different seed => different hash.
    let sched = schedule();
    let cfg1 = MatchConfig { seed: 1, ..MatchConfig::default() };
    let cfg2 = MatchConfig { seed: 2, ..MatchConfig::default() };

    let a = run_scripted_match(cfg1, vec![card("sentinel.ron"), card("vanguard.ron")], vec![card("pyromancer.ron")], &sched);
    let b = run_scripted_match(cfg2, vec![card("sentinel.ron"), card("vanguard.ron")], vec![card("pyromancer.ron")], &sched);

    assert_ne!(match_hash(&a), match_hash(&b), "seed must affect the state hash");
}
