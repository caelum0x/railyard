//! The authoritative game server.
//!
//! Split into:
//! - [`authoritative`] — the [`Server`]: owns the truth, buffers + applies
//!   validated inputs at their effect tick, emits snapshots (the M0/M1 spine),
//! - [`validate`]      — game-level input authorization (ownership / liveness),
//! - [`snapshot`]      — deterministic snapshot delta computation for telemetry.

pub mod authoritative;
pub mod snapshot;
pub mod validate;

pub use authoritative::Server;
