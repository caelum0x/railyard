//! `simrun` — deterministic CLI harness for the sim (the "bevy-harness" stand-in).
//!
//! This is the scriptable control surface Claude Code (or a human) can drive to
//! build / run-headless / step N ticks / dump a state hash. It is deliberately a
//! plain CLI with no dependencies; an MCP wrapper can shell out to it later.
//!
//! Usage:
//!   cargo run -p sim --bin simrun -- --seed 42 --ticks 1000 --hash
//!   cargo run -p sim --bin simrun -- --seed 7 --ticks 300          # prints state
//!   cargo run -p sim --bin simrun -- --help
//!
//! Flags:
//!   --seed <u64>     RNG seed (default 42)
//!   --ticks <u64>    Number of ticks to run (default 1000)
//!   --hash           Print only the final u64 state hash (for scripting)
//!   --target x,y     Click-to-move target applied at tick 0 (default 20,15)
//!   --help           Show this help

use std::process::ExitCode;

use sim::{state_hash, step, Input, Vec2, World};

struct Args {
    seed: u64,
    ticks: u64,
    hash_only: bool,
    target: Vec2,
}

fn parse_args() -> Result<Args, String> {
    let mut seed = 42u64;
    let mut ticks = 1000u64;
    let mut hash_only = false;
    let mut target = Vec2::new(20.0, 15.0);

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--help" | "-h" => return Err("help".to_string()),
            "--hash" => hash_only = true,
            "--seed" => {
                let v = it.next().ok_or("--seed requires a value")?;
                seed = v.parse().map_err(|_| format!("invalid --seed: {v}"))?;
            }
            "--ticks" => {
                let v = it.next().ok_or("--ticks requires a value")?;
                ticks = v.parse().map_err(|_| format!("invalid --ticks: {v}"))?;
            }
            "--target" => {
                let v = it.next().ok_or("--target requires x,y")?;
                let (xs, ys) = v.split_once(',').ok_or("--target must be x,y")?;
                let x = xs.parse().map_err(|_| format!("invalid target x: {xs}"))?;
                let y = ys.parse().map_err(|_| format!("invalid target y: {ys}"))?;
                target = Vec2::new(x, y);
            }
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    Ok(Args {
        seed,
        ticks,
        hash_only,
        target,
    })
}

const HELP: &str = "\
simrun — deterministic MOBA sim harness

USAGE:
    simrun [--seed <u64>] [--ticks <u64>] [--target x,y] [--hash]

FLAGS:
    --seed <u64>     RNG seed (default 42)
    --ticks <u64>    Ticks to run (default 1000)
    --target x,y     Click-to-move target applied at tick 0 (default 20,15)
    --hash           Print only the final u64 state hash
    -h, --help       Show this help
";

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) if e == "help" => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("error: {e}\n\n{HELP}");
            return ExitCode::FAILURE;
        }
    };

    let mut world = World::new(args.seed);

    for t in 0..args.ticks {
        let inputs: Vec<Input> = if t == 0 {
            vec![Input::move_to(0, args.target)]
        } else {
            Vec::new()
        };
        step(&mut world, &inputs);
    }

    let hash = state_hash(&world);
    if args.hash_only {
        println!("{hash}");
    } else {
        let champ = world
            .champion(0)
            .expect("champion 0 always exists in this scenario");
        println!("seed={} ticks={}", args.seed, args.ticks);
        println!("final_tick={}", world.tick);
        println!("champion0.pos=({:.6}, {:.6})", champ.pos.x, champ.pos.y);
        println!("champion0.target={:?}", champ.target);
        println!("state_hash={hash}");
    }

    ExitCode::SUCCESS
}
