//! ROSTER-WIDE integration test for the full champion pool (baseline examples +
//! the mythology roster authored as data in `specs/`).
//!
//! Two guarantees, both purely data-driven so the roster scales by authoring:
//!   1. EVERY `specs/*.ron` LOADS and semantically VALIDATES — the single gate that
//!      keeps a malformed champion out of the game.
//!   2. The spec-driven balance tournament (a round-robin of `run_duel`) is fully
//!      DETERMINISTIC: the same specs + the same seed set produce byte-identical
//!      results on a re-run. This is what makes the auto-balance signal trustworthy.

use std::fs;
use std::path::{Path, PathBuf};

use champions::{load_file, run_duel, ChampionSpec, DuelConfig, DuelResult};

const SPECS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../content/champions");

/// Fixed seed set used for the deterministic round-robin.
const SEEDS: &[u64] = &[1, 7, 13, 42, 101];

/// The 24 mythology champions this roster milestone shipped, by spec slug.
const MYTHOLOGY_SLUGS: &[&str] = &[
    // Greek
    "zeus", "hades", "athena", "medusa", "achilles", "artemis",
    // Norse
    "tyr", "thor", "odin", "loki", "freyja", "ullr",
    // Roman
    "mars", "vulcan", "bellona", "neptune", "mercury", "janus",
    // Egyptian
    "ra-radiant-dawn", "anubis-warden-of-scales", "horus-skyfall-talon",
    "isis-mother-of-magic", "set-lord-of-chaos", "bastet-claws-of-the-hunt",
];

/// All `.ron` spec files in the specs directory, sorted for stable ordering.
///
/// The `roster_meta.ron` sidecar (3D asset pipeline metadata: archetype/faction
/// per champion) lives alongside the specs but is NOT a `ChampionSpec`, so it is
/// excluded here just as the asset-validation test (`assets.rs`) excludes it.
fn spec_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = fs::read_dir(SPECS_DIR)
        .expect("specs dir must exist")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "ron").unwrap_or(false))
        .filter(|p| p.file_stem().and_then(|s| s.to_str()) != Some("roster_meta"))
        .collect();
    paths.sort();
    paths
}

fn slug(path: &Path) -> String {
    path.file_stem().unwrap().to_string_lossy().to_string()
}

/// Load + validate every spec, returning them in stable (sorted-path) order.
fn load_all() -> Vec<(String, ChampionSpec)> {
    spec_paths()
        .iter()
        .map(|p| {
            let spec = load_file(p)
                .unwrap_or_else(|e| panic!("spec {} must load + validate: {e}", p.display()));
            (slug(p), spec)
        })
        .collect()
}

// --- 1. every spec loads + validates ----------------------------------------

#[test]
fn every_spec_loads_and_validates() {
    let specs = load_all();
    // Baseline examples (3) + the 24 mythology champions are all present.
    assert!(
        specs.len() >= MYTHOLOGY_SLUGS.len(),
        "expected at least {} specs, found {}",
        MYTHOLOGY_SLUGS.len(),
        specs.len()
    );
    for (slug, spec) in &specs {
        assert!(!spec.name.trim().is_empty(), "{slug} has empty name");
        assert!(spec.base.hp > 0, "{slug} has non-positive hp");
    }
}

// --- every mythology champion is present and loads --------------------------

#[test]
fn mythology_roster_is_complete() {
    let specs = load_all();
    for slug in MYTHOLOGY_SLUGS {
        assert!(
            specs.iter().any(|(s, _)| s == slug),
            "mythology champion '{slug}' is missing from specs/"
        );
    }
}

// --- spec slugs are unique (no duplicate champions) -------------------------

#[test]
fn spec_slugs_are_unique() {
    let paths = spec_paths();
    let mut slugs: Vec<String> = paths.iter().map(|p| slug(p)).collect();
    slugs.sort();
    let before = slugs.len();
    slugs.dedup();
    assert_eq!(before, slugs.len(), "duplicate spec slugs detected");
}

/// Run the full round-robin (each champ vs every other, over the fixed seed set)
/// and collect an assertable fingerprint per matchup.
fn round_robin(specs: &[(String, ChampionSpec)]) -> Vec<DuelResult> {
    let mut results = Vec::new();
    for i in 0..specs.len() {
        for j in 0..specs.len() {
            if i == j {
                continue;
            }
            for &seed in SEEDS {
                let cfg = DuelConfig {
                    seed,
                    ..DuelConfig::default()
                };
                results.push(run_duel(&specs[i].1, &specs[j].1, cfg));
            }
        }
    }
    results
}

// --- 2. the round-robin tournament is deterministic -------------------------

#[test]
fn round_robin_tournament_is_deterministic() {
    let specs = load_all();
    let first = round_robin(&specs);
    let second = round_robin(&specs);
    assert_eq!(
        first.len(),
        specs.len() * (specs.len() - 1) * SEEDS.len(),
        "unexpected number of duels"
    );
    assert_eq!(
        first, second,
        "round-robin diverged across two runs with identical specs + seeds"
    );
}

// --- a single duel is deterministic across repeated runs --------------------

#[test]
fn every_matchup_is_individually_deterministic() {
    let specs = load_all();
    // Spot-check determinism at the individual-duel granularity for a handful of
    // representative mythology matchups (fast, and pins the per-duel contract).
    let pick = |slug: &str| {
        specs
            .iter()
            .find(|(s, _)| s == slug)
            .map(|(_, spec)| spec.clone())
            .unwrap_or_else(|| panic!("missing spec {slug}"))
    };
    let pairs = [
        ("zeus", "tyr"),
        ("loki", "athena"),
        ("set-lord-of-chaos", "sentinel"),
    ];
    for (a, b) in pairs {
        let sa = pick(a);
        let sb = pick(b);
        for &seed in SEEDS {
            let cfg = DuelConfig {
                seed,
                ..DuelConfig::default()
            };
            let r1 = run_duel(&sa, &sb, cfg);
            let r2 = run_duel(&sa, &sb, cfg);
            assert_eq!(r1, r2, "{a} vs {b} @seed {seed} not deterministic");
            assert_eq!(r1.final_hash, r2.final_hash, "hash mismatch {a} vs {b}");
        }
    }
}
