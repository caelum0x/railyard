//! Predicting client (the prediction half of the netcode spine).
//!
//! The client runs the *same* deterministic `sim` as the server. It **predicts** —
//! applies the local player's inputs immediately (no waiting for the server) and
//! steps its local world each tick. The **reconcile** half lives in
//! [`super::reconcile`]; together they are the M1 predict/reconcile algorithm the
//! `no_desync` gate proves never diverges.
//!
//! Fields the reconcile half needs are `pub(crate)` so the sibling module can
//! adopt authoritative truth and replay pending inputs.

use sim::net::StampedInput;
use sim::{step, Input, World};

/// A client that predicts locally and reconciles to authoritative snapshots.
pub struct Client {
    pub world: World,
    /// Inputs generated locally but not yet acknowledged by a server snapshot.
    pub(crate) pending: Vec<StampedInput>,
    pub(crate) next_seq: u64,
}

impl Client {
    /// Create a client world from a seed. Must match the server's seed.
    pub fn new(seed: u64) -> Self {
        Client {
            world: World::new(seed),
            pending: Vec::new(),
            next_seq: 1,
        }
    }

    /// Number of inputs awaiting acknowledgement (for tests/telemetry).
    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Register a locally generated input for the current client tick. Returns a
    /// [`StampedInput`] the caller sends to the server (the transport may rewrite
    /// `effect_tick` to a future server tick to model input delay). The client
    /// itself predicts the input immediately at the current tick.
    pub fn generate_input(&mut self, input: Input) -> StampedInput {
        let stamped = StampedInput {
            seq: self.next_seq,
            effect_tick: self.world.tick,
            input,
        };
        self.next_seq += 1;
        self.pending.push(stamped);
        stamped
    }

    /// Advance local prediction by one tick, applying any pending inputs whose
    /// `effect_tick` is the current tick.
    pub fn predict(&mut self) {
        let now = self.world.tick;
        let inputs = self.inputs_at(now);
        step(&mut self.world, &inputs);
    }

    /// Collect pending inputs scheduled to take effect at `tick`, in sequence
    /// order (deterministic). Shared by `predict` and the reconcile replay.
    pub(crate) fn inputs_at(&self, tick: u64) -> Vec<Input> {
        let mut due: Vec<&StampedInput> = self
            .pending
            .iter()
            .filter(|s| s.effect_tick == tick)
            .collect();
        due.sort_by_key(|s| s.seq);
        due.into_iter().map(|s| s.input).collect()
    }
}
