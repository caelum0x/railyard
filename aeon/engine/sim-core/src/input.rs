//! Input intents fed into the deterministic sim.
//!
//! The sim's public step function consumes a slice of `Input`. Netcode concerns
//! (sequence numbers, tick stamps, latency) live in the `net` module and in the
//! server/client crates, keeping the sim engine- and transport-agnostic.

use crate::math::Vec2;

/// Stable identifier for a simulated entity. Champions are ordered by id.
pub type EntityId = u32;

/// A single command an entity wants to perform this tick.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Command {
    /// Click-to-move: set a destination the champion walks toward (with arrive).
    MoveTo(Vec2),
    /// Cast the champion's skillshot ability toward a world-space aim point. A
    /// projectile spawns at the caster's position travelling toward the aim point;
    /// it is gated by the caster's ability cooldown (see `systems::apply_inputs`).
    CastSkillshot(Vec2),
}

/// An intent to apply to a specific entity.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Input {
    pub entity: EntityId,
    pub command: Command,
}

impl Input {
    pub fn move_to(entity: EntityId, target: Vec2) -> Self {
        Input {
            entity,
            command: Command::MoveTo(target),
        }
    }

    /// Convenience constructor for a skillshot cast aimed at `aim`.
    pub fn cast_skillshot(entity: EntityId, aim: Vec2) -> Self {
        Input {
            entity,
            command: Command::CastSkillshot(aim),
        }
    }
}
