//! Transport-agnostic netcode value types shared by server and client.
//!
//! These describe *what* is sent, not *how*. Real UDP wiring (bevy_replicon /
//! lightyear) is the M1.5 step; for M0+M1 the predict/reconcile algorithm is
//! proven in-process against these types (see `client/tests/no_desync.rs`).

use crate::input::Input;
use crate::world::World;

/// An input tagged with a monotonically increasing sequence number and the tick
/// at which it takes effect. On the client, `effect_tick` is the client tick the
/// input was generated on (immediate prediction). When forwarded to the server,
/// the harness rewrites `effect_tick` to a slightly future server tick (input
/// delay) so the authoritative sim applies it deterministically on arrival.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StampedInput {
    pub seq: u64,
    pub effect_tick: u64,
    pub input: Input,
}

/// An authoritative snapshot broadcast by the server. Carries the full world plus
/// the highest input sequence number the server has applied, so the client can
/// drop acknowledged inputs and replay only the still-pending ones.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub world: World,
    pub last_applied_seq: u64,
}
