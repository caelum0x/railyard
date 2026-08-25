//! Authoritative input validation.
//!
//! The sim already clamps move/aim points into the play-plane
//! ([`sim::systems::validate_input`]). This module layers the *game-level* checks a
//! server must enforce before trusting a client command: a player may only command
//! the entity they own, and only while it is alive. These are pure functions over
//! read-only state, so they are deterministic and side-effect free.

use sim::{Input, World};

/// Why a submitted input was rejected by the authoritative server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RejectReason {
    /// The input targets an entity the sender does not own.
    NotOwner,
    /// The commanded entity does not exist in the world.
    NoSuchEntity,
    /// The commanded entity is dead (dead units ignore all input).
    EntityDead,
}

/// Check that `input`, submitted by the player owning `owned_entity`, is legal
/// against the current `world`. Returns the point-clamped, sim-validated input on
/// success, or the [`RejectReason`] on failure.
pub fn authorize(
    world: &World,
    owned_entity: sim::EntityId,
    input: Input,
) -> Result<Input, RejectReason> {
    if input.entity != owned_entity {
        return Err(RejectReason::NotOwner);
    }
    match world.champion(input.entity) {
        None => Err(RejectReason::NoSuchEntity),
        Some(c) if !c.alive => Err(RejectReason::EntityDead),
        Some(_) => Ok(sim::systems::validate_input(input)),
    }
}
