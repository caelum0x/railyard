//! UMBRAL FACTION TESTS — the auto-verification gate for the Umbral champions.
//!
//! The Umbral faction (shadow/eclipse/void — the Erebos Lab brand) completes the
//! original fantasy roster with five champions spanning five archetypes: a tank
//! (Dregmaw), a diving bruiser (Korrathil), a burst mage (Xelphine), a marksman
//! (Quorvane), and an assassin (Zeryth). Like the rest of the roster they are
//! authored purely as DATA against the closed DSL, so this suite is the green/red
//! signal that they are safe and balanced:
//!
//!   * every Umbral spec LOADS and VALIDATES (structural + semantic gate),
//!   * Umbral slugs and display names are unique — both among themselves and
//!     against the shipped fantasy roster + the Sentinel baseline,
//!   * the round-robin (each Umbral champion vs every fantasy-roster champion +
//!     vs the Sentinel baseline) is fully DETERMINISTIC across two runs, per
//!     pairing and by final state hash,
//!   * NO Umbral champion is a degenerate extreme: each one's win-rate over that
//!     round-robin sits within [20%, 80%], locking in the balance pass.
//!
//! Adding or retuning an Umbral champion is editing DATA; this test re-proves the
//! faction without any human combat-code review.

use champions::{load_file, run_duel, ChampionSpec, DuelConfig};

const SPECS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../content/champions");

/// The Umbral faction, by spec slug — the five champions under test.
const UMBRAL: &[&str] = &["dregmaw", "korrathil", "xelphine", "quorvane", "zeryth"];

/// The shipped fantasy roster (Ember/Tide/Verdant) — the balance opponents pool.
const FANTASY_ROSTER: &[&str] = &[
    // Ember
    "toran", "vashka", "ysmera", "kaeza", "nerreth",
    // Tide
    "brackmoor", "nyxralla", "cyranth", "veyl", "lumisel",
    // Verdant
    "mosscloak", "gorehoof", "sporeveil", "thornquill", "petalsong",
];

/// Baseline every champion is measured against.
const BASELINE: &str = "sentinel";

/// Fixed seeds threaded through the world — the same set the roster test uses.
/// (Combat itself is RNG-free, but the seed is part of world state + hash, so we
/// exercise several for a faithful determinism contract.)
const SEEDS: &[u64] = &[1, 7, 42, 100, 2024];

fn load(slug: &str) -> ChampionSpec {
    load_file(format!("{SPECS_DIR}/{slug}.ron"))
        .unwrap_or_else(|e| panic!("Umbral/roster spec '{slug}' must load + validate: {e}"))
}

/// The full opponents pool for the balance round-robin: fantasy roster + baseline.
fn load_opponents() -> Vec<ChampionSpec> {
    FANTASY_ROSTER
        .iter()
        .chain(std::iter::once(&BASELINE))
        .map(|s| load(s))
        .collect()
}

// --- every Umbral spec loads + validates ------------------------------------

#[test]
fn every_umbral_spec_loads_and_validates() {
    for slug in UMBRAL {
        let spec = load(slug);
        assert!(!spec.name.trim().is_empty(), "{slug}: empty name");
        assert!(
            !spec.abilities.is_empty(),
            "{slug}: Umbral champions must have abilities"
        );
        for ab in &spec.abilities {
            assert!(!ab.effects.is_empty(), "{slug}/{}: empty effects", ab.name);
        }
    }
    assert_eq!(UMBRAL.len(), 5, "Umbral faction size is locked at 5");
}

// --- Umbral slugs + names are unique, incl. against the shipped roster --------

#[test]
fn umbral_slugs_and_names_are_unique() {
    // Slugs unique among the Umbral faction itself.
    let mut umbral_slugs: Vec<&&str> = UMBRAL.iter().collect();
    umbral_slugs.sort();
    umbral_slugs.dedup();
    assert_eq!(umbral_slugs.len(), UMBRAL.len(), "duplicate Umbral slug");

    // Slugs unique against the fantasy roster + baseline (no collisions on disk).
    let mut all_slugs: Vec<&str> = UMBRAL
        .iter()
        .chain(FANTASY_ROSTER.iter())
        .chain(std::iter::once(&BASELINE))
        .copied()
        .collect();
    let total = all_slugs.len();
    all_slugs.sort();
    all_slugs.dedup();
    assert_eq!(
        all_slugs.len(),
        total,
        "Umbral slug collides with an existing roster/baseline slug"
    );

    // Display names unique across the whole combined cast.
    let mut names: Vec<String> = UMBRAL
        .iter()
        .chain(FANTASY_ROSTER.iter())
        .chain(std::iter::once(&BASELINE))
        .map(|s| load(s).name)
        .collect();
    let total_names = names.len();
    names.sort();
    names.dedup();
    assert_eq!(
        names.len(),
        total_names,
        "duplicate champion display name across the combined cast"
    );
}

// --- the Umbral round-robin is deterministic --------------------------------

#[test]
fn umbral_round_robin_is_deterministic() {
    let opponents = load_opponents();
    for slug in UMBRAL {
        let champ = load(slug);
        for opp in &opponents {
            for &seed in SEEDS {
                let cfg = DuelConfig {
                    seed,
                    ..DuelConfig::default()
                };
                // Both slot orderings, twice each, must reproduce exactly.
                for (a, b) in [(&champ, opp), (opp, &champ)] {
                    let r0 = run_duel(a, b, cfg);
                    let r1 = run_duel(a, b, cfg);
                    assert_eq!(
                        r0, r1,
                        "duel {} vs {} @seed {seed} diverged across two runs",
                        a.name, b.name
                    );
                    assert_eq!(
                        r0.final_hash, r1.final_hash,
                        "final hash for {} vs {} @seed {seed} diverged",
                        a.name, b.name
                    );
                }
            }
        }
    }
}

// --- no Umbral champion is a degenerate extreme (locks the balance pass) ------

#[test]
fn no_umbral_champion_is_a_balance_extreme() {
    let opponents = load_opponents();

    for slug in UMBRAL {
        let champ = load(slug);
        let mut wins = 0u64;
        let mut games = 0u64;

        // Each opponent, both slot orderings, across every seed.
        for opp in &opponents {
            for &seed in SEEDS {
                let cfg = DuelConfig {
                    seed,
                    ..DuelConfig::default()
                };
                // Umbral champion in slot 0.
                let r0 = run_duel(&champ, opp, cfg);
                games += 1;
                if r0.winner == Some(0) {
                    wins += 1;
                }
                // Umbral champion in slot 1.
                let r1 = run_duel(opp, &champ, cfg);
                games += 1;
                if r1.winner == Some(1) {
                    wins += 1;
                }
            }
        }

        assert!(games > 0, "{slug}: played no games");
        // Inclusive [20%, 80%] band, integer math to avoid float drift.
        assert!(
            wins * 100 <= games * 80,
            "{slug} is TOO STRONG: {wins}/{games} = {:.1}% win-rate (>80%)",
            wins as f64 / games as f64 * 100.0
        );
        assert!(
            wins * 100 >= games * 20,
            "{slug} is TOO WEAK: {wins}/{games} = {:.1}% win-rate (<20%)",
            wins as f64 / games as f64 * 100.0
        );
    }
}
