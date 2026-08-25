//! `duel` — spec-driven duel CLI. The scriptable green/red gate for a new champion.
//!
//! Usage:
//!   cargo run -p champions --bin duel -- <spec_a.ron> <spec_b.ron> [--seed N] [--ticks N] [--distance D]
//!
//! Loads + validates both specs, runs a deterministic duel, and prints the winner,
//! remaining HP, tick count, and final state hash. Exit code is 0 on a decisive
//! result, 2 on a draw/timeout, 1 on a load/validation error.

use std::process::ExitCode;

use champions::{load_file, run_duel, DuelConfig};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let (Some(path_a), Some(path_b)) = (args.next(), args.next()) else {
        eprintln!("usage: duel <spec_a.ron> <spec_b.ron> [--seed N] [--ticks N] [--distance D]");
        return ExitCode::FAILURE;
    };

    let mut cfg = DuelConfig::default();
    while let Some(flag) = args.next() {
        let val = args.next();
        match (flag.as_str(), val) {
            ("--seed", Some(v)) => cfg.seed = v.parse().unwrap_or(cfg.seed),
            ("--ticks", Some(v)) => cfg.max_ticks = v.parse().unwrap_or(cfg.max_ticks),
            ("--distance", Some(v)) => cfg.start_distance = v.parse().unwrap_or(cfg.start_distance),
            (other, _) => {
                eprintln!("unknown or incomplete flag: {other}");
                return ExitCode::FAILURE;
            }
        }
    }

    let spec_a = match load_file(&path_a) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to load {path_a}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let spec_b = match load_file(&path_b) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("failed to load {path_b}: {e}");
            return ExitCode::FAILURE;
        }
    };

    let result = run_duel(&spec_a, &spec_b, cfg);
    println!("duel: {} (id 0) vs {} (id 1)", spec_a.name, spec_b.name);
    println!("seed={} start_distance={}", cfg.seed, cfg.start_distance);
    match &result.winner_name {
        Some(name) => println!("winner: {name} (id {})", result.winner.unwrap()),
        None => println!("winner: DRAW"),
    }
    for (id, hp) in &result.hp {
        println!("  champion {id}: hp={hp}");
    }
    println!("ticks={} final_hash={}", result.ticks, result.final_hash);

    if result.winner.is_some() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(2)
    }
}
