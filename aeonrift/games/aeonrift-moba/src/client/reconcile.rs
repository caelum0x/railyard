//! Server reconciliation (the reconcile half of the netcode spine).
//!
//! When an authoritative snapshot arrives, the client resets its world to that
//! snapshot, discards inputs the server has acknowledged, and re-applies (replays)
//! the still-pending inputs to re-derive the present, corrected state. Because the
//! sim is deterministic, replaying the same inputs from the snapshot reproduces the
//! server's trajectory, so client and server converge with zero drift.
//!
//! Implemented as an inherent method on [`super::prediction::Client`] in this
//! sibling module — the reconcile logic is genuinely separate from prediction, so
//! it lives in its own file while sharing the client's `pub(crate)` internals.

use sim::net::Snapshot;
use sim::step;

use super::prediction::Client;

impl Client {
    /// Reconcile against an authoritative snapshot: reset to it, drop acked inputs,
    /// and replay the remaining pending inputs up to the present tick.
    pub fn reconcile(&mut self, snapshot: &Snapshot) {
        let current = self.world.tick;
        let snap_tick = snapshot.world.tick;
        if snap_tick > current {
            // Snapshot is newer than our prediction (shouldn't happen with a
            // forward-only server); ignore to stay safe.
            return;
        }

        // Adopt authoritative truth.
        self.world = snapshot.world.clone();
        // Forget everything the server has already applied.
        self.pending.retain(|s| s.seq > snapshot.last_applied_seq);

        // Replay still-pending inputs deterministically from the snapshot tick
        // back up to where our prediction was.
        while self.world.tick < current {
            let now = self.world.tick;
            let inputs = self.inputs_at(now);
            step(&mut self.world, &inputs);
        }
    }
}
