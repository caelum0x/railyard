//! Headless authoritative server demo.
//!
//! Runs the authoritative sim for N ticks with a couple of scripted, validated
//! inputs and prints the final state hash. This is the server-side analogue of
//! `simrun` and doubles as a smoke test that the server crate links the sim.
//!
//! Usage: cargo run -p server -- [seed] [ticks]

use aeonrift_moba::Server;
use sim::net::StampedInput;
use sim::{state_hash, Input, Vec2};

fn main() {
    let mut args = std::env::args().skip(1);
    let seed: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(42);
    let ticks: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1000);

    let mut server = Server::new(seed);

    // Scripted inputs: an in-bounds move, then an out-of-bounds move that the
    // server clamps to the play-plane (demonstrating authoritative validation).
    server.receive(StampedInput {
        seq: 1,
        effect_tick: 10,
        input: Input::move_to(0, Vec2::new(20.0, 15.0)),
    });
    server.receive(StampedInput {
        seq: 2,
        effect_tick: 60,
        input: Input::move_to(0, Vec2::new(200.0, 0.0)), // clamped to (50, 0)
    });

    for _ in 0..ticks {
        server.tick();
    }

    let champ = server.world.champion(0).expect("champion 0 exists");
    println!("[server] seed={seed} ticks={ticks}");
    println!("[server] final_tick={}", server.world.tick);
    println!(
        "[server] champion0.pos=({:.6}, {:.6})",
        champ.pos.x, champ.pos.y
    );
    println!("[server] last_applied_seq={}", server.last_applied_seq);
    println!("[server] state_hash={}", state_hash(&server.world));
}
