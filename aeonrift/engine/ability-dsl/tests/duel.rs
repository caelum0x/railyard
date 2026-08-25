//! M5 SPEC-DRIVEN DUEL TESTS — the auto-verification gate.
//!
//! Covers:
//!   * DETERMINISM: same specs + seed => byte-identical duel result, twice,
//!   * a SENSIBLE, EXACT outcome: the burst mage beats the plain auto-attacker with
//!     an exact remaining-HP assertion (locks in a regression baseline),
//!   * the bruiser's dash+strike also beats the auto-attacker,
//!   * result independence from champion ordering (winner is the same champion when
//!     the two specs are swapped).

use champions::ChampionSpec;
use champions::{load_file, run_duel, DuelConfig};

const SPECS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../content/champions");

fn load(name: &str) -> ChampionSpec {
    load_file(format!("{SPECS_DIR}/{name}")).expect("example spec must load + validate")
}

const SEED: u64 = 42;

// --- determinism: same specs + seed => identical result, twice --------------

#[test]
fn duel_is_deterministic_same_specs_and_seed() {
    let mage = load("pyromancer.ron");
    let auto = load("sentinel.ron");
    let cfg = DuelConfig {
        seed: SEED,
        ..DuelConfig::default()
    };

    let a = run_duel(&mage, &auto, cfg);
    let b = run_duel(&mage, &auto, cfg);

    assert_eq!(
        a, b,
        "duel diverged across two runs with the same specs + seed"
    );
    assert_eq!(a.final_hash, b.final_hash, "final state hash diverged");
}

// --- sensible + EXACT outcome: burst mage beats the plain auto-attacker ------

#[test]
fn burst_mage_beats_auto_attacker_with_exact_hp() {
    let mage = load("pyromancer.ron");
    let auto = load("sentinel.ron");
    let cfg = DuelConfig {
        seed: SEED,
        ..DuelConfig::default()
    };

    let result = run_duel(&mage, &auto, cfg);

    // The mage (id 0) wins.
    assert_eq!(result.winner, Some(0), "burst mage should win");
    assert_eq!(result.winner_name.as_deref(), Some("Pyromancer"));

    // EXACT remaining HP, baked from the deterministic sim (regression baseline).
    assert_eq!(result.hp_of(0), 102, "mage's exact remaining HP");
    assert!(result.hp_of(1) <= 0, "auto-attacker must be dead");
    assert_eq!(result.hp_of(1), -82, "auto-attacker's exact HP at death");

    // Decisive within a sane tick budget.
    assert_eq!(result.ticks, 292, "exact tick count of the decisive duel");
}

// --- outcome is independent of authoring order ------------------------------

#[test]
fn winner_is_stable_when_specs_are_swapped() {
    let mage = load("pyromancer.ron");
    let auto = load("sentinel.ron");
    let cfg = DuelConfig {
        seed: SEED,
        ..DuelConfig::default()
    };

    let normal = run_duel(&mage, &auto, cfg);
    let swapped = run_duel(&auto, &mage, cfg);

    // Same champion wins either way; the winner is Pyromancer in both.
    assert_eq!(normal.winner_name, Some("Pyromancer".to_string()));
    assert_eq!(swapped.winner_name, Some("Pyromancer".to_string()));
    // The mage's winning HP is identical regardless of which slot it occupies.
    assert_eq!(normal.hp_of(0), swapped.hp_of(1));
}

// --- bruiser dash+strike also beats the auto-attacker -----------------------

#[test]
fn bruiser_beats_auto_attacker() {
    let bruiser = load("vanguard.ron");
    let auto = load("sentinel.ron");
    let cfg = DuelConfig {
        seed: SEED,
        ..DuelConfig::default()
    };

    let result = run_duel(&bruiser, &auto, cfg);
    assert_eq!(result.winner_name.as_deref(), Some("Vanguard"));
    assert!(result.hp_of(0) > 0, "bruiser survives");
    assert!(result.hp_of(1) <= 0, "auto-attacker is dead");
}
