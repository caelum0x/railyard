//! FANTASY ROSTER TESTS — the auto-verification gate for the shipped roster.
//!
//! An original 15-champion roster across four factions (Umbral/Ember/Tide/Verdant)
//! is authored purely as DATA against the closed DSL. This suite is the green/red
//! signal that the roster is safe and balanced:
//!
//!   * every roster spec LOADS and VALIDATES (structural + semantic gate),
//!   * the round-robin (each champion vs every other + vs the Sentinel baseline) is
//!     fully DETERMINISTIC — identical results across two runs, per pairing and in
//!     aggregate (state-hash fingerprints included),
//!   * NO champion is a degenerate extreme: every champion's round-robin win-rate
//!     sits within [20%, 80%], locking in the balance pass as a regression baseline.
//!
//! Adding or retuning a champion is editing DATA; this test re-proves the whole
//! roster without any human combat-code review.

use champions::{load_file, run_duel, ChampionSpec, DuelConfig};

const SPECS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../content/champions");

/// The shipped fantasy roster, by spec slug. Ember, Tide, and Verdant factions.
/// (The Umbral entry from the source list was an invalid placeholder and was
/// dropped at validation time — see the integration report.)
const ROSTER: &[&str] = &[
    // Ember
    "toran", "vashka", "ysmera", "kaeza", "nerreth",
    // Tide
    "brackmoor", "nyxralla", "cyranth", "veyl", "lumisel",
    // Verdant
    "mosscloak", "gorehoof", "sporeveil", "thornquill", "petalsong",
];

/// Baseline every champion is measured against.
const BASELINE: &str = "sentinel";

/// Fixed seeds threaded through the world (combat itself is RNG-free, but the seed
/// is part of world state + hash, so we exercise several for a faithful contract).
const SEEDS: &[u64] = &[1, 7, 42, 100, 2024];

fn load(slug: &str) -> ChampionSpec {
    load_file(format!("{SPECS_DIR}/{slug}.ron"))
        .unwrap_or_else(|e| panic!("roster spec '{slug}' must load + validate: {e}"))
}

/// Load the whole roster plus the baseline (opponents pool), in a stable order.
fn load_all() -> Vec<ChampionSpec> {
    ROSTER
        .iter()
        .chain(std::iter::once(&BASELINE))
        .map(|s| load(s))
        .collect()
}

// --- every roster spec loads + validates ------------------------------------

#[test]
fn every_roster_spec_loads_and_validates() {
    for slug in ROSTER {
        let spec = load(slug);
        assert!(!spec.name.trim().is_empty(), "{slug}: empty name");
        assert!(
            !spec.abilities.is_empty(),
            "{slug}: roster champions must have abilities"
        );
        for ab in &spec.abilities {
            assert!(!ab.effects.is_empty(), "{slug}/{}: empty effects", ab.name);
        }
    }
    assert_eq!(ROSTER.len(), 15, "roster size is locked at 15");
}

// --- roster slugs and champion names are unique -----------------------------

#[test]
fn roster_slugs_and_names_are_unique() {
    let mut slugs: Vec<&&str> = ROSTER.iter().collect();
    slugs.sort();
    slugs.dedup();
    assert_eq!(slugs.len(), ROSTER.len(), "duplicate roster slug");

    let specs = load_all();
    let mut names: Vec<String> = specs.iter().map(|s| s.name.clone()).collect();
    names.sort();
    names.dedup();
    assert_eq!(
        names.len(),
        specs.len(),
        "duplicate champion display name in roster"
    );
}

// --- the full round-robin is deterministic ----------------------------------

#[test]
fn round_robin_is_deterministic() {
    let specs = load_all();
    let n = specs.len();

    for i in 0..n {
        for j in 0..n {
            if i == j {
                continue;
            }
            for &seed in SEEDS {
                let cfg = DuelConfig {
                    seed,
                    ..DuelConfig::default()
                };
                let a = run_duel(&specs[i], &specs[j], cfg);
                let b = run_duel(&specs[i], &specs[j], cfg);
                assert_eq!(
                    a, b,
                    "duel {i} vs {j} @seed {seed} diverged across two runs"
                );
                assert_eq!(
                    a.final_hash, b.final_hash,
                    "final hash for {i} vs {j} @seed {seed} diverged"
                );
            }
        }
    }
}

// --- no champion is a degenerate extreme (locks the balance pass) ------------

#[test]
fn no_champion_is_a_balance_extreme() {
    let specs = load_all();
    let n = specs.len();
    let mut wins = vec![0u32; n];
    let mut games = vec![0u32; n];

    // Each ordered pairing, both slots, across every seed.
    for i in 0..n {
        for j in 0..n {
            if i == j {
                continue;
            }
            for &seed in SEEDS {
                let cfg = DuelConfig {
                    seed,
                    ..DuelConfig::default()
                };
                let r = run_duel(&specs[i], &specs[j], cfg);
                games[i] += 1;
                games[j] += 1;
                match r.winner {
                    Some(0) => wins[i] += 1,
                    Some(1) => wins[j] += 1,
                    _ => {} // draw: counts as a game for both, a win for neither
                }
            }
        }
    }

    // Only the roster itself is balance-gated (the Sentinel baseline is excluded).
    for (k, slug) in ROSTER.iter().enumerate() {
        let (w, g) = (wins[k], games[k]);
        assert!(g > 0, "{slug}: played no games");
        // Inclusive [20%, 80%] band, integer math to avoid float drift:
        //   wins/games <= 0.80  <=>  wins*100 <= games*80
        //   wins/games >= 0.20  <=>  wins*100 >= games*20
        assert!(
            w as u64 * 100 <= g as u64 * 80,
            "{slug} is TOO STRONG: {w}/{g} = {:.1}% win-rate (>80%)",
            w as f64 / g as f64 * 100.0
        );
        assert!(
            w as u64 * 100 >= g as u64 * 20,
            "{slug} is TOO WEAK: {w}/{g} = {:.1}% win-rate (<20%)",
            w as f64 / g as f64 * 100.0
        );
    }
}
