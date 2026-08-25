//! Deterministic unit movement AI: advance, or hold to fight.
//!
//! Deployed units auto-advance toward the enemy base. The rule is Clash-simple and
//! fully deterministic: if a living enemy (an enemy unit OR the enemy base) is within
//! the unit's auto-attack range, it HOLDS position so the sim's auto-attack system can
//! fire; otherwise it MARCHES toward the enemy base. Bases never move.
//!
//! Combat itself is NOT decided here — it is entirely the sim's `auto_attack_system`
//! and `projectile_system`. This module only produces movement `Input`s, in ascending
//! entity-id order, which is what keeps the whole battler deterministic.

use sim::{EntityId, Input, World};

use super::deploy::march_input;
use super::match_state::MatchState;

/// Does `unit` have any living enemy within its auto-attack range right now?
fn enemy_in_range(world: &World, unit_id: EntityId) -> bool {
    let Some(me) = world.champion(unit_id) else {
        return false;
    };
    world.champions.iter().any(|other| {
        other.id != me.id
            && other.alive
            && other.team != me.team
            && me.pos.distance(other.pos) <= me.attack_range
    })
}

/// Compute this tick's movement inputs for every living, deployed unit, in ascending
/// entity-id order (deterministic). Bases and dead units produce no input.
///
/// * enemy in range  -> hold (issue a move to the unit's own position: it arrives
///   immediately and stops, letting the auto-attack system take over),
/// * otherwise       -> march toward the enemy base.
pub fn movement_inputs(m: &MatchState) -> Vec<Input> {
    let mut inputs = Vec::new();
    // `world.champions` is maintained in ascending id order, so this iteration is
    // deterministic without an explicit sort.
    for champ in &m.world.champions {
        if m.is_base(champ.id) || !champ.alive {
            continue;
        }
        if enemy_in_range(&m.world, champ.id) {
            // Hold: target our own current position => movement_system snaps + stops.
            inputs.push(Input::move_to(champ.id, champ.pos));
        } else {
            inputs.push(march_input(m, champ.id, champ.team));
        }
    }
    inputs
}
