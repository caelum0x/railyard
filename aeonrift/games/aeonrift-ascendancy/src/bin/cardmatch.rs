//! `cardmatch` — the scripted bot-vs-bot match runner for Aeonrift: Ascendancy.
//!
//! Loads champion specs from the shared roster, turns them into unit cards, and plays
//! a deterministic bot-vs-bot lane battle to a base-destroyed win condition. Prints
//! the winner, both bases' HP, tick count, and the card-game state hash.
//!
//! Usage:
//!   cargo run -p aeonrift-ascendancy --bin cardmatch -- [--seed N] [--interval N] [--ticks N]
//!
//! Exit code: 0 on a decisive result, 2 on a draw/timeout, 1 on a spec load error.

use std::process::ExitCode;

use cards::{match_hash, run_bot_match, Card, MatchConfig, Outcome, DEFAULT_DEPLOY_INTERVAL};
use champions::load_file;

const SPECS_DIR: &str = cards::CONTENT_CHAMPIONS_DIR;

fn load_card(file: &str) -> Result<Card, String> {
    let path = format!("{SPECS_DIR}/{file}");
    let spec = load_file(&path).map_err(|e| format!("failed to load {path}: {e}"))?;
    Ok(Card::from_spec(spec))
}

fn main() -> ExitCode {
    let mut cfg = MatchConfig::default();
    let mut interval = DEFAULT_DEPLOY_INTERVAL;

    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let val = args.next();
        match (flag.as_str(), val) {
            ("--seed", Some(v)) => cfg.seed = v.parse().unwrap_or(cfg.seed),
            ("--interval", Some(v)) => interval = v.parse().unwrap_or(interval),
            ("--ticks", Some(v)) => cfg.max_ticks = v.parse().unwrap_or(cfg.max_ticks),
            (other, _) => {
                eprintln!("unknown or incomplete flag: {other}");
                return ExitCode::FAILURE;
            }
        }
    }

    // Asymmetric decks so the match is decisive: player 0 fields the durable
    // auto-attacker + the bruiser; player 1 fields only the fragile burst mage.
    let (sentinel, vanguard, pyromancer) = match (
        load_card("sentinel.ron"),
        load_card("vanguard.ron"),
        load_card("pyromancer.ron"),
    ) {
        (Ok(s), Ok(v), Ok(p)) => (s, v, p),
        (r1, r2, r3) => {
            for r in [r1, r2, r3] {
                if let Err(e) = r {
                    eprintln!("{e}");
                }
            }
            return ExitCode::FAILURE;
        }
    };

    println!("Aeonrift: Ascendancy — deterministic lane card-battler");
    println!(
        "seed={} lanes={} elixir_regen={}t interval={}t",
        cfg.seed, cfg.lanes, cfg.elixir_regen_period, interval
    );
    println!("card costs: {}={}  {}={}  {}={}",
        sentinel.name(), sentinel.meta.cost,
        vanguard.name(), vanguard.meta.cost,
        pyromancer.name(), pyromancer.meta.cost,
    );

    let deck0 = vec![sentinel, vanguard];
    let deck1 = vec![pyromancer];

    let m = run_bot_match(cfg, deck0, deck1, interval);

    match m.outcome {
        Some(Outcome::Winner(p)) => println!("WINNER: player {p} (destroyed the enemy base)"),
        Some(Outcome::Draw) => println!("RESULT: DRAW"),
        None => println!("RESULT: unresolved"),
    }
    println!(
        "base HP:  player 0 = {}   player 1 = {}",
        m.base_hp(0),
        m.base_hp(1)
    );
    println!(
        "units deployed: player 0 = {}   player 1 = {}",
        m.deployed_count[0], m.deployed_count[1]
    );
    println!("ticks={} match_hash={}", m.world.tick, match_hash(&m));

    match m.outcome {
        Some(Outcome::Winner(_)) => ExitCode::SUCCESS,
        _ => ExitCode::from(2),
    }
}
