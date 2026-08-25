//! (g) PRESET-MATCH gate — a full seeded match with a preset deck ends deterministically.
//!
//! A bot fielding one of the 12 shipped preset decks marches down the lanes and
//! destroys an undefended enemy base; the result (winner, tick, hash) is reproducible
//! across identical runs. Also: two preset decks played head-to-head resolve
//! deterministically.

use cards::deck::load_default_presets;
use cards::{match_hash, run_bot_match, Card, MatchConfig, Outcome, DEFAULT_DEPLOY_INTERVAL};

fn preset(name: &str) -> Vec<Card> {
    let decks = load_default_presets().expect("preset decks load");
    decks
        .into_iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("preset deck {name} not found"))
        .cards
}

#[test]
fn preset_deck_destroys_an_undefended_base_deterministically() {
    let cfg = MatchConfig::default();
    // Player 0 fields a real preset deck; player 1 has no cards to defend with, so a
    // base-destroyed win is guaranteed — the point is that it is DECISIVE + reproducible.
    let deck0 = preset("Greek Olympians");

    let a = run_bot_match(cfg, deck0.clone(), vec![], DEFAULT_DEPLOY_INTERVAL);
    let b = run_bot_match(cfg, deck0, vec![], DEFAULT_DEPLOY_INTERVAL);

    assert_eq!(a.outcome, Some(Outcome::Winner(0)), "preset deck should win");
    assert!(!a.base_alive(1), "enemy base is destroyed");
    assert!(a.base_alive(0), "winner base survives");
    assert!(a.world.tick < cfg.max_ticks, "won before the tick cap");
    assert!(a.deployed_count[0] > 0, "player 0 fielded units");

    // Determinism: identical runs => identical tick + full-state hash.
    assert_eq!(a.world.tick, b.world.tick);
    assert_eq!(match_hash(&a), match_hash(&b));
    assert_eq!(a, b);
}

#[test]
fn two_preset_decks_resolve_deterministically() {
    let cfg = MatchConfig::default();
    let a = run_bot_match(
        cfg,
        preset("Norse Aesir"),
        preset("Roman Legion"),
        DEFAULT_DEPLOY_INTERVAL,
    );
    let b = run_bot_match(
        cfg,
        preset("Norse Aesir"),
        preset("Roman Legion"),
        DEFAULT_DEPLOY_INTERVAL,
    );
    assert_eq!(a.outcome, b.outcome, "same matchup => same outcome");
    assert_eq!(match_hash(&a), match_hash(&b), "same matchup => same hash");
}
