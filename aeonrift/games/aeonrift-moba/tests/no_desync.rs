//! NETCODE SPINE GATE (M1).
//!
//! An authoritative server and a predicting client are wired together in-process.
//! Local inputs reach the server over a channel with injected latency + jitter,
//! and snapshots return the same way. Over >=1000 ticks the reconciling client
//! must converge to byte-identical state with the server (zero desync).
//!
//! To prove reconciliation is actually load-bearing (not a no-op), the scenario
//! includes an out-of-bounds move that the server clamps (authoritative
//! validation). A control run with a NON-reconciling client is shown to diverge,
//! while the reconciling client converges.
//!
//! Real UDP transport (bevy_replicon / lightyear) is the documented M1.5 step;
//! this harness proves the predict/reconcile ALGORITHM deterministically.

use aeonrift_moba::{Client, Server};
use sim::net::{Snapshot, StampedInput};
use sim::rng::Rng;
use sim::{state_hash, Input, Vec2};

const SEED: u64 = 42;
const TICKS: u64 = 1500;
// Input delay: the server applies an input this many ticks after the client
// generated it. Must exceed max one-way latency so inputs always arrive before
// their effect tick (deterministic authoritative application).
const INPUT_DELAY: u64 = 10;
const BASE_LATENCY: u64 = 3;
const MAX_JITTER: u64 = 4; // so max one-way latency = 7 < INPUT_DELAY = 10
                           // Distinct seeds so uplink and downlink jitter streams are independent.
const UPLINK_SEED: u64 = 0xA1B2_C3D4;
const DOWNLINK_SEED: u64 = 0x5566_7788;

/// A message in flight, delivered when the global tick reaches `arrival`.
struct InFlight<T> {
    arrival: u64,
    payload: T,
}

/// One-way delay channel with seeded jitter.
struct Channel<T> {
    rng: Rng,
    queue: Vec<InFlight<T>>,
}

impl<T> Channel<T> {
    fn new(seed: u64) -> Self {
        Channel {
            rng: Rng::new(seed),
            queue: Vec::new(),
        }
    }

    /// Send `payload` at global tick `now`; it arrives after base + jitter ticks.
    fn send(&mut self, now: u64, payload: T) {
        let jitter = self.rng.next_u64() % (MAX_JITTER + 1);
        let arrival = now + BASE_LATENCY + jitter;
        self.queue.push(InFlight { arrival, payload });
    }

    /// Drain everything that has arrived by global tick `now`.
    fn drain_arrived(&mut self, now: u64) -> Vec<T> {
        let mut ready = Vec::new();
        let mut remaining = Vec::new();
        for m in self.queue.drain(..) {
            if m.arrival <= now {
                ready.push(m.payload);
            } else {
                remaining.push(m);
            }
        }
        self.queue = remaining;
        ready
    }
}

/// Scripted local clicks: (tick, raw target). The second target is out of bounds
/// so the server clamps it to (50, 0), forcing a real authoritative correction.
fn scripted_click(tick: u64) -> Option<Vec2> {
    match tick {
        20 => Some(Vec2::new(20.0, 15.0)),
        60 => Some(Vec2::new(200.0, 0.0)), // clamped by server to (50, 0)
        _ => None,
    }
}

/// The M1 gate.
#[test]
fn no_desync_converges_under_latency_and_jitter() {
    let mut server = Server::new(SEED);
    let mut client = Client::new(SEED);

    let mut uplink: Channel<StampedInput> = Channel::new(UPLINK_SEED);
    let mut downlink: Channel<Snapshot> = Channel::new(DOWNLINK_SEED);

    // How many of the final ticks must match exactly, tick-by-tick.
    const TAIL: u64 = 200;
    let mut tail_mismatches = 0u64;

    for t in 0..TICKS {
        // ---- Client: generate input (predicted immediately) and send it up ----
        if let Some(raw_target) = scripted_click(t) {
            let stamped = client.generate_input(Input::move_to(0, raw_target));
            // Rewrite effect_tick to a future SERVER tick (input delay). The
            // client keeps predicting at its own (earlier) effect_tick.
            let for_server = StampedInput {
                effect_tick: t + INPUT_DELAY,
                ..stamped
            };
            uplink.send(t, for_server);
        }
        client.predict();

        // ---- Deliver inputs to server; advance authoritative sim ----
        for msg in uplink.drain_arrived(t) {
            server.receive(msg);
        }
        server.tick();

        // ---- Server broadcasts a snapshot; deliver any that have arrived ----
        downlink.send(t, server.snapshot());
        let snapshots = downlink.drain_arrived(t);
        if let Some(latest) = snapshots.into_iter().max_by_key(|s| s.world.tick) {
            client.reconcile(&latest);
        }

        // ---- Tail convergence check: last TAIL ticks must match every tick ----
        if t >= TICKS - TAIL && state_hash(&client.world) != state_hash(&server.world) {
            tail_mismatches += 1;
        }
    }

    // Final byte-identical convergence (the primary gate).
    assert_eq!(
        client.world, server.world,
        "client and server worlds diverged at the end"
    );
    assert_eq!(
        state_hash(&client.world),
        state_hash(&server.world),
        "final state hashes differ"
    );
    // Zero drift across the whole settled tail.
    assert_eq!(
        tail_mismatches, 0,
        "client desynced from server on {tail_mismatches} of the last {TAIL} ticks"
    );
    // All inputs acknowledged => nothing left pending.
    assert_eq!(client.pending_len(), 0, "client still has unacked inputs");

    // Sanity: the authoritative server clamped the out-of-bounds target.
    let champ = server.world.champion(0).unwrap();
    assert_eq!(
        champ.pos,
        Vec2::new(50.0, 0.0),
        "server did not clamp/settle at the expected authoritative position"
    );
}

/// Control: a client that NEVER reconciles keeps its raw (unclamped) prediction
/// and ends at a different final state than the authoritative server. This proves
/// the reconciliation in the gate above is doing real work.
#[test]
fn naive_client_without_reconciliation_diverges() {
    let mut server = Server::new(SEED);
    let mut client = Client::new(SEED);
    let mut uplink: Channel<StampedInput> = Channel::new(UPLINK_SEED);

    for t in 0..TICKS {
        if let Some(raw_target) = scripted_click(t) {
            let stamped = client.generate_input(Input::move_to(0, raw_target));
            let for_server = StampedInput {
                effect_tick: t + INPUT_DELAY,
                ..stamped
            };
            uplink.send(t, for_server);
        }
        client.predict(); // predicts raw target, never reconciled

        for msg in uplink.drain_arrived(t) {
            server.receive(msg);
        }
        server.tick();
    }

    // The naive client walked to the unclamped (200, 0) target; the server
    // clamped to (50, 0). They must differ.
    assert_ne!(
        state_hash(&client.world),
        state_hash(&server.world),
        "naive client unexpectedly matched the server; the test scenario no \
         longer forces a divergence, so it cannot prove reconciliation matters"
    );
}
