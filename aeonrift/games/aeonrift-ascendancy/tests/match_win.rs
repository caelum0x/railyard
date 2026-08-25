//! (c) WIN-CONDITION gate.
//!
//! A full scripted bot-vs-bot match reaches a base-destroyed win condition
//! deterministically, with an asserted winner. Asymmetric decks (player 0 fields the
//! durable auto-attacker + bruiser; player 1 only the fragile burst mage) make the
//! outcome decisive rather than a mirror draw.

use cards::{run_bot_match, Card, MatchConfig, Outcome, DEFAULT_DEPLOY_INTERVAL};
use champions::{load_file, ChampionSpec};

const SPECS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../content/champions");

fn spec(name: &str) -> ChampionSpec {
    load_file(format!("{SPECS_DIR}/{name}")).expect("roster spec must load + validate")
}

fn card(name: &str) -> Card {
    Card::from_spec(spec(name))
}

fn decks() -> (Vec<Card>, Vec<Card>) {
    (
        vec![card("sentinel.ron"), card("vanguard.ron")],
        vec![card("pyromancer.ron")],
    )
}

#[test]
fn bot_match_reaches_base_destroyed_win() {
    let cfg = MatchConfig::default();
    let (d0, d1) = decks();
    let m = run_bot_match(cfg, d0, d1, DEFAULT_DEPLOY_INTERVAL);

    // Decisive: player 0 wins by destroying player 1's base.
    assert_eq!(m.outcome, Some(Outcome::Winner(0)), "player 0 should win");
    assert!(m.base_alive(0), "winner's base survives");
    assert!(!m.base_alive(1), "loser's base is destroyed");
    assert!(m.base_hp(1) <= 0, "loser base HP is depleted");
    assert!(m.base_hp(0) > 0, "winner base HP remains");

    // The win happened before the tick cap (a real base-destroyed end, not a timeout).
    assert!(m.world.tick < cfg.max_ticks, "resolved before the tick cap");

    // Both players actually fielded units (the economy let the match play out).
    assert!(m.deployed_count[0] > 0 && m.deployed_count[1] > 0);
}

#[test]
fn bot_match_outcome_is_a_deterministic_regression_baseline() {
    // Exact fingerprints baked from the deterministic runner (regression guard, same
    // style as the champion duel harness). If combat/economy tuning changes, these
    // update together — but they must never differ between two runs.
    let cfg = MatchConfig::default();
    let (d0, d1) = decks();
    let m = run_bot_match(cfg, d0, d1, DEFAULT_DEPLOY_INTERVAL);

    assert_eq!(m.outcome, Some(Outcome::Winner(0)));
    assert_eq!(m.world.tick, 1840, "exact decisive tick");
    assert_eq!(m.base_hp(0), 2216, "winner base exact remaining HP");
    assert_eq!(m.base_hp(1), -30, "loser base exact HP at destruction");
}
