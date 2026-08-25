//! Authoritative server: the single source of truth.
//!
//! The server runs the shared `sim`, buffers incoming (delayed) inputs, applies
//! each one at its scheduled `effect_tick` after validating it, and emits
//! snapshots that acknowledge the highest applied sequence number. It never reads
//! the wall-clock and never rewinds — authority is forward-only.

use sim::net::{Snapshot, StampedInput};
use sim::systems::validate_input;
use sim::{step, Input, World};

/// The authoritative game server for one match.
pub struct Server {
    pub world: World,
    /// Highest input sequence number applied so far (monotonic).
    pub last_applied_seq: u64,
    /// Inputs received but not yet due (their `effect_tick` is in the future).
    buffer: Vec<StampedInput>,
}

impl Server {
    /// Create a server world from a seed. Must match the client's seed so both
    /// sims start from byte-identical initial state.
    pub fn new(seed: u64) -> Self {
        Server {
            world: World::new(seed),
            last_applied_seq: 0,
            buffer: Vec::new(),
        }
    }

    /// Accept an input off the wire. It will be applied when the world reaches
    /// the input's `effect_tick`. Duplicates (same seq) are ignored.
    pub fn receive(&mut self, msg: StampedInput) {
        if msg.effect_tick < self.world.tick {
            // Arrived too late to be applied at its scheduled tick. A real server
            // would drop or fast-forward it; here the harness guarantees inputs
            // arrive before their effect tick, so this is defensive only.
            return;
        }
        if self.buffer.iter().any(|b| b.seq == msg.seq) {
            return;
        }
        self.buffer.push(msg);
    }

    /// Advance the authoritative world by one tick, applying any buffered inputs
    /// whose `effect_tick` equals the current tick (validated, in sequence order).
    pub fn tick(&mut self) {
        let now = self.world.tick;

        let mut due: Vec<StampedInput> = self
            .buffer
            .iter()
            .copied()
            .filter(|b| b.effect_tick == now)
            .collect();
        due.sort_by_key(|b| b.seq);

        let inputs: Vec<Input> = due.iter().map(|b| validate_input(b.input)).collect();
        for b in &due {
            if b.seq > self.last_applied_seq {
                self.last_applied_seq = b.seq;
            }
        }
        // Drop everything we just applied (and any stale leftovers).
        self.buffer.retain(|b| b.effect_tick > now);

        step(&mut self.world, &inputs);
    }

    /// Produce an authoritative snapshot of the current state.
    pub fn snapshot(&self) -> Snapshot {
        Snapshot {
            world: self.world.clone(),
            last_applied_seq: self.last_applied_seq,
        }
    }
}
