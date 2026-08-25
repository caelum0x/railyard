//! MOBA slice integration tests.
//!
//! These exercise the real rules/economy/AI stack (not just compilation): champions
//! load from content into playable units, waves spawn on cadence, a deterministic
//! bot pushes a lane and damages a turret, and a full siege destroys the enemy nexus
//! to end the match — reproducibly from a seed.

use aeonrift_moba::ai;
use aeonrift_moba::game::Match;
use aeonrift_moba::modes::{normal, GameMode};
use aeonrift_moba::rules::{RED};
use aeonrift_moba::champions;
use sim::Vec2;

/// Team-size-1 practice config on a given map (single-lane test maps).
fn solo(map: &'static str) -> GameMode {
    GameMode::practice(map, 1, false)
}

#[test]
fn champions_load_from_content_into_playable_units() {
    let roster = champions::load_roster().expect("roster loads");
    assert!(
        roster.len() >= 40,
        "expected the full authored roster, got {}",
        roster.len()
    );
    // Sorted by slug (deterministic draft order).
    let mut sorted = roster.clone();
    sorted.sort_by(|a, b| a.slug.cmp(&b.slug));
    assert_eq!(
        roster.iter().map(|e| &e.slug).collect::<Vec<_>>(),
        sorted.iter().map(|e| &e.slug).collect::<Vec<_>>(),
        "roster must be in sorted slug order"
    );

    // Compile one spec into a live sim champion and check the base stats carried over.
    let entry = &roster[0];
    let champ = champions::playable(&entry.spec, 7, 0, Vec2::new(1.0, 2.0));
    assert_eq!(champ.id, 7);
    assert_eq!(champ.team, 0);
    assert_eq!(champ.health.max, entry.spec.base.hp, "hp compiles through");
    assert_eq!(champ.attack_damage, entry.spec.base.ad, "ad compiles through");
    assert!(champ.alive);
}

#[test]
fn normal_mode_builds_a_full_arena() {
    let m = Match::new(normal::config(), 42).expect("standard match builds");
    // 5v5 champions + 3 lanes * (4 turrets + 2 inhibitors) + 2 nexus + jungle(2) + objectives(2).
    let champs = m
        .registry
        .iter()
        .filter(|u| matches!(u.kind, aeonrift_moba::rules::UnitKind::Champion))
        .count();
    assert_eq!(champs, 10, "5v5 champions drafted");
    let nexuses = m
        .registry
        .iter()
        .filter(|u| u.kind == aeonrift_moba::rules::UnitKind::Nexus)
        .count();
    assert_eq!(nexuses, 2, "one nexus per team");
    assert!(m.winner().is_none(), "match starts with no winner");
}

#[test]
fn waves_spawn_on_cadence() {
    let mut m = Match::new(solo("proving_lane"), 7).expect("proving lane builds");
    let before = count_minions(&m);
    assert_eq!(before, 0, "no minions before the first wave");

    // Step past the first wave tick (first_spawn = 30).
    for _ in 0..40 {
        m.step(&[]);
    }
    let after = count_minions(&m);
    assert!(after > 0, "a minion wave spawned by tick 40, got {after}");
    // Both teams field minions.
    let blue = count_team_minions(&m, 0);
    let red = count_team_minions(&m, 1);
    assert!(blue > 0 && red > 0, "both teams spawn minions (blue={blue}, red={red})");
}

#[test]
fn bot_push_damages_an_enemy_turret() {
    let mut m = Match::new(solo("proving_lane"), 11).expect("builds");
    let goal = m.map.nexus_pos(RED).expect("red nexus exists");

    let turret_id = enemy_turret_id(&m);
    let full_hp = m.world.champion(turret_id).unwrap().health.max;

    // Drive BLUE champion 0 with the laning bot for a while; it should push into the
    // enemy turret (helped by its minions) and chip it.
    let mut min_turret_hp = full_hp;
    for _ in 0..3000 {
        let inputs = ai::decide(&m.world, &m.registry, 0, goal);
        m.step(&inputs);
        if let Some(t) = m.world.champion(turret_id) {
            min_turret_hp = min_turret_hp.min(t.health.current);
        }
        if m.winner().is_some() {
            break;
        }
    }
    assert!(
        min_turret_hp < full_hp,
        "the enemy turret should have taken damage (min {min_turret_hp} / {full_hp})"
    );
}

#[test]
fn nexus_destroyed_ends_the_match() {
    let (winner, end_tick, fp) = run_sandbox_to_end(99);
    assert_eq!(winner, Some(0), "BLUE sieges down the RED nexus and wins");
    assert!(end_tick > 0, "the match ran before ending");

    // Determinism: an identical seed reproduces the exact same result.
    let (winner2, end_tick2, fp2) = run_sandbox_to_end(99);
    assert_eq!(winner, winner2, "winner is reproducible");
    assert_eq!(end_tick, end_tick2, "end tick is reproducible");
    assert_eq!(fp, fp2, "final fingerprint is byte-identical across runs");
}

#[test]
fn match_is_deterministic_tick_by_tick() {
    let mut a = Match::new(solo("proving_lane"), 2024).expect("builds");
    let mut b = Match::new(solo("proving_lane"), 2024).expect("builds");
    for _ in 0..1200 {
        let goal = a.map.nexus_pos(RED).unwrap();
        let ia = ai::decide(&a.world, &a.registry, 0, goal);
        let ib = ai::decide(&b.world, &b.registry, 0, goal);
        a.step(&ia);
        b.step(&ib);
        assert_eq!(
            a.fingerprint(),
            b.fingerprint(),
            "two same-seed matches diverged at tick {}",
            a.world.tick
        );
    }
}

// ---- helpers ---------------------------------------------------------------

fn count_minions(m: &Match) -> usize {
    m.registry
        .iter()
        .filter(|u| matches!(u.kind, aeonrift_moba::rules::UnitKind::Minion(_)))
        .count()
}

fn count_team_minions(m: &Match, team: u8) -> usize {
    m.registry
        .iter()
        .filter(|u| matches!(u.kind, aeonrift_moba::rules::UnitKind::Minion(_)) && u.team == team)
        .count()
}

fn enemy_turret_id(m: &Match) -> sim::EntityId {
    m.registry
        .iter()
        .find(|u| u.kind == aeonrift_moba::rules::UnitKind::Turret && u.team == RED)
        .map(|u| u.id)
        .expect("an enemy turret exists")
}

/// Run the sandbox_duel map with BLUE champ 0 sieging the RED base until the match
/// ends (or a tick cap). Returns (winner, end_tick, final_fingerprint).
fn run_sandbox_to_end(seed: u64) -> (Option<u8>, u64, u64) {
    let mut m = Match::new(solo("sandbox_duel"), seed).expect("sandbox builds");
    let goal = m.map.nexus_pos(RED).expect("red nexus exists");
    let cap = 8000;
    for _ in 0..cap {
        let inputs = ai::decide(&m.world, &m.registry, 0, goal);
        m.step(&inputs);
        if m.winner().is_some() {
            break;
        }
    }
    (m.winner(), m.world.tick, m.fingerprint())
}
