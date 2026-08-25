//! Client-side input buffering.
//!
//! A small deterministic ring of the most recent local commands, plus a helper
//! that stamps a raw [`sim::Input`] into a [`StampedInput`] destined for the
//! authoritative server with a given input delay. Used by the bot-driven and
//! scripted clients (see `ai`, `modes`) so they share one stamping convention.

use sim::net::StampedInput;
use sim::Input;

/// A bounded, deterministic history of recently issued inputs (newest last).
///
/// Kept for prediction debugging / replay-style tooling: the buffer never uses a
/// clock or a hash map, so iterating it is order-stable.
#[derive(Clone, Debug, Default)]
pub struct InputBuffer {
    capacity: usize,
    entries: Vec<StampedInput>,
}

impl InputBuffer {
    /// A buffer holding at most `capacity` recent inputs (0 == unbounded).
    pub fn new(capacity: usize) -> Self {
        InputBuffer {
            capacity,
            entries: Vec::new(),
        }
    }

    /// Record an input, evicting the oldest if at capacity.
    pub fn push(&mut self, input: StampedInput) {
        self.entries.push(input);
        if self.capacity != 0 && self.entries.len() > self.capacity {
            self.entries.remove(0);
        }
    }

    /// The inputs in issue order (oldest first).
    pub fn iter(&self) -> impl Iterator<Item = &StampedInput> {
        self.entries.iter()
    }

    /// How many inputs are buffered.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if no inputs are buffered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Stamp a raw input generated at `client_tick` so the authoritative server applies
/// it `input_delay` ticks later (the delay must exceed max one-way latency so the
/// input always arrives before its effect tick — see the `no_desync` harness).
pub fn stamp_for_server(seq: u64, client_tick: u64, input_delay: u64, input: Input) -> StampedInput {
    StampedInput {
        seq,
        effect_tick: client_tick + input_delay,
        input,
    }
}
