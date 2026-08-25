//! A simple, deterministic laning bot.
//!
//! Behaviour each tick (pure function of world state — no clock, no RNG):
//! 1. If an enemy is within skillshot range and the ability is off cooldown, fire a
//!    skillshot at it (reusing the sim's built-in skillshot).
//! 2. Movement: hold position to auto-attack if an enemy is inside attack range;
//!    else approach the nearest aggroed enemy; else attack-move toward `goal` (the
//!    enemy nexus), pushing the lane.
//!
//! Returns the [`sim::Input`]s for `bot_id`. Order is stable (move then cast).

use sim::{EntityId, Input, Vec2, World, ABILITY_RANGE};

use crate::rules::lanes::{nearest_enemy, AGGRO_RADIUS};
use crate::rules::Registry;

/// Decide this tick's inputs for the champion `bot_id`, pushing toward `goal`.
pub fn decide(world: &World, registry: &Registry, bot_id: EntityId, goal: Vec2) -> Vec<Input> {
    let Some(bot) = world.champion(bot_id) else {
        return Vec::new();
    };
    if !bot.alive || bot.stun_ticks > 0 {
        return Vec::new();
    }
    let (pos, team, range) = (bot.pos, bot.team, bot.attack_range);
    let mut inputs = Vec::new();

    // Movement decision from the nearest aggroed enemy (structures + enemy units).
    match nearest_enemy(world, registry, team, pos, AGGRO_RADIUS, false) {
        Some((_, _epos, edist)) if edist <= range => {
            inputs.push(Input::move_to(bot_id, pos)); // hold to auto-attack
        }
        Some((_, epos, _)) => {
            inputs.push(Input::move_to(bot_id, epos)); // approach
        }
        None => {
            inputs.push(Input::move_to(bot_id, goal)); // push the lane
        }
    }

    // Ability: cast a skillshot at the nearest enemy in ability range if ready.
    if bot.ability_cooldown == 0 {
        if let Some((_, epos, _)) =
            nearest_enemy(world, registry, team, pos, ABILITY_RANGE, false)
        {
            inputs.push(Input::cast_skillshot(bot_id, epos));
        }
    }

    inputs
}
