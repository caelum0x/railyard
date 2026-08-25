//! Fog-of-war / vision.
//!
//! An entity is visible to a team if it is allied, or if it stands within
//! [`VISION_RADIUS`] of any living allied unit. Queries iterate the sim's champion
//! vec (already sorted by id), so results are deterministic and order-stable.

use sim::{EntityId, Vec2, World};

use super::Registry;

/// How far each living unit reveals the map around itself.
pub const VISION_RADIUS: f32 = 12.0;

/// Positions of every living unit on `team` (its vision sources).
fn vision_sources(world: &World, team: u8) -> Vec<Vec2> {
    world
        .champions
        .iter()
        .filter(|c| c.alive && c.team == team)
        .map(|c| c.pos)
        .collect()
}

/// Is `target_id` currently visible to `team`?
pub fn is_visible(team: u8, target_id: EntityId, world: &World, _registry: &Registry) -> bool {
    let Some(target) = world.champion(target_id) else {
        return false;
    };
    if target.team == team {
        return true; // always see your own units
    }
    let sources = vision_sources(world, team);
    sources
        .iter()
        .any(|src| src.distance(target.pos) <= VISION_RADIUS)
}

/// All entity ids currently visible to `team`, in ascending id order.
pub fn visible_to(team: u8, world: &World, _registry: &Registry) -> Vec<EntityId> {
    let sources = vision_sources(world, team);
    world
        .champions
        .iter()
        .filter(|c| {
            c.team == team
                || sources.iter().any(|src| src.distance(c.pos) <= VISION_RADIUS)
        })
        .map(|c| c.id)
        .collect()
}
