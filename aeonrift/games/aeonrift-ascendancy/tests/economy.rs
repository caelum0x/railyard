//! (b) ECONOMY rules gate.
//!
//! * You cannot deploy a card you cannot afford (explicit `NotEnoughElixir`).
//! * A successful deploy spends exactly the card's cost.
//! * Elixir regenerates deterministically: +1 per fixed period, capped at the max.

use cards::{
    match_hash, regen_elixir, step_tick, try_deploy, Card, Deploy, DeployError, MatchConfig,
    MatchState,
};
use champions::{load_file, ChampionSpec};

const SPECS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../content/champions");

fn spec(name: &str) -> ChampionSpec {
    load_file(format!("{SPECS_DIR}/{name}")).expect("roster spec must load + validate")
}

fn card(name: &str) -> Card {
    Card::from_spec(spec(name))
}

#[test]
fn cannot_deploy_without_enough_elixir() {
    // Sentinel costs 4; start elixir is 5. First deploy ok (5 -> 1), second rejected.
    let cfg = MatchConfig::default();
    let sentinel = card("sentinel.ron");
    assert_eq!(sentinel.meta.cost, 4, "cost precondition for this test");

    let mut m = MatchState::new(cfg, vec![sentinel], vec![]);
    assert_eq!(m.elixir[0], cfg.start_elixir);

    // First deploy succeeds and spends exactly the cost.
    let id = try_deploy(&mut m, Deploy::new(0, 0, 0)).expect("first deploy affordable");
    assert_eq!(id, cards::FIRST_UNIT_ID, "first unit gets the first unit id");
    assert_eq!(m.elixir[0], 1, "cost of 4 spent from 5");
    assert_eq!(m.deployed_count[0], 1);

    // Second deploy is unaffordable (have 1, need 4) and is REJECTED, unchanged state.
    let before = m.clone();
    let err = try_deploy(&mut m, Deploy::new(0, 0, 0)).unwrap_err();
    assert_eq!(err, DeployError::NotEnoughElixir { have: 1, need: 4 });
    assert_eq!(m, before, "a rejected deploy must not mutate state");
}

#[test]
fn bad_card_and_lane_are_rejected() {
    let cfg = MatchConfig::default();
    let mut m = MatchState::new(cfg, vec![card("pyromancer.ron")], vec![]);

    assert_eq!(
        try_deploy(&mut m, Deploy::new(0, 5, 0)).unwrap_err(),
        DeployError::BadCard
    );
    assert_eq!(
        try_deploy(&mut m, Deploy::new(0, 0, 99)).unwrap_err(),
        DeployError::BadLane
    );
    assert_eq!(
        try_deploy(&mut m, Deploy::new(3, 0, 0)).unwrap_err(),
        DeployError::BadPlayer
    );
}

#[test]
fn elixir_regenerates_deterministically_and_caps() {
    // Direct test of the pure regen rule: it fires only on a positive multiple of the
    // period, adds exactly 1 per player, and never exceeds the max.
    let cfg = MatchConfig::default();
    let mut m = MatchState::new(cfg, vec![], vec![]);
    assert_eq!(m.elixir, [cfg.start_elixir, cfg.start_elixir]);

    // Not a regen tick: no change.
    m.world.tick = 1;
    regen_elixir(&mut m);
    assert_eq!(m.elixir, [5, 5], "no regen off the period boundary");

    // A regen tick: +1 each.
    m.world.tick = cfg.elixir_regen_period;
    regen_elixir(&mut m);
    assert_eq!(m.elixir, [6, 6], "one regen on the period boundary");

    // Drive elixir up to the cap and confirm it never exceeds it.
    for k in 2..20 {
        m.world.tick = cfg.elixir_regen_period * k;
        regen_elixir(&mut m);
    }
    assert_eq!(m.elixir, [cfg.max_elixir, cfg.max_elixir], "elixir caps at the max");
}

#[test]
fn regen_via_step_tick_matches_the_rule() {
    // Integration: stepping the match with no deploys regenerates elixir on schedule.
    let cfg = MatchConfig::default();
    let mut m = MatchState::new(cfg, vec![], vec![]);

    // Step until just before the first regen boundary: still at start elixir.
    while m.world.tick < cfg.elixir_regen_period {
        step_tick(&mut m, &[]);
    }
    assert_eq!(m.world.tick, cfg.elixir_regen_period);
    assert_eq!(m.elixir[0], cfg.start_elixir, "no regen before the boundary step");

    // One more step crosses the boundary (entry tick == period): +1.
    step_tick(&mut m, &[]);
    assert_eq!(m.elixir[0], cfg.start_elixir + 1, "regen after crossing the boundary");

    // Regen is part of the hashed, deterministic state.
    let h = match_hash(&m);
    assert_ne!(h, 0);
}
