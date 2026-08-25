//! Deterministic minion lane AI: follow the lane path toward the enemy base, but
//! stop to attack when an enemy is in range.
//!
//! This produces [`sim::Input`]s (click-to-move) for every living minion, applied
//! by the sim like any other input — so minion movement and combat run through the
//! exact same deterministic systems as champions. Iteration is over the registry in
//! id order, so the emitted input order is stable.

use sim::{EntityId, Input, Vec2, World};

use super::map::MapData;
use super::{Registry, UnitKind};

/// A minion (or bot) engages an enemy within this radius.
pub const AGGRO_RADIUS: f32 = 11.0;
/// Distance at which a minion considers a waypoint "reached" and advances.
pub const WAYPOINT_TOLERANCE: f32 = 1.5;

/// The nearest living enemy of `team` to `from`, within `radius`. Enemies are units
/// on the other player team (and neutrals iff `include_neutral`). Ties break to the
/// lowest id (stable order). Returns (id, pos, distance).
pub fn nearest_enemy(
    world: &World,
    registry: &Registry,
    team: u8,
    from: Vec2,
    radius: f32,
    include_neutral: bool,
) -> Option<(EntityId, Vec2, f32)> {
    let mut best: Option<(EntityId, Vec2, f32)> = None;
    for c in &world.champions {
        if !c.alive || c.team == team {
            continue;
        }
        if c.team == super::NEUTRAL && !include_neutral {
            continue;
        }
        // Only count entities the registry actually knows about (defensive).
        if registry.get(c.id).is_none() {
            continue;
        }
        let d = from.distance(c.pos);
        if d <= radius && best.map(|(_, _, bd)| d < bd).unwrap_or(true) {
            best = Some((c.id, c.pos, d));
        }
    }
    best
}

/// Emit a move command for every living minion: hold-and-attack if an enemy is in
/// its own attack range, approach an aggroed enemy, else march the lane. Advances
/// each minion's waypoint cursor in `registry`.
pub fn steer_minions(map: &MapData, registry: &mut Registry, world: &World) -> Vec<Input> {
    // Collect minion ids in id order first, then process (so we can mutate registry
    // waypoint cursors while reading the world immutably).
    let minion_ids: Vec<EntityId> = registry
        .iter()
        .filter(|u| matches!(u.kind, UnitKind::Minion(_)))
        .map(|u| u.id)
        .collect();

    let mut inputs = Vec::with_capacity(minion_ids.len());
    for id in minion_ids {
        let Some(champ) = world.champion(id) else {
            continue;
        };
        if !champ.alive {
            continue;
        }
        let (pos, team, range) = (champ.pos, champ.team, champ.attack_range);

        // Engage if an enemy is within aggro radius.
        if let Some((_, epos, edist)) = nearest_enemy(world, registry, team, pos, AGGRO_RADIUS, false)
        {
            if edist <= range {
                inputs.push(Input::move_to(id, pos)); // hold to auto-attack
            } else {
                inputs.push(Input::move_to(id, epos)); // approach
            }
            continue;
        }

        // Otherwise march the lane path.
        if let Some(target) = advance_lane(map, registry, id, pos) {
            inputs.push(Input::move_to(id, target));
        }
    }
    inputs
}

/// Advance a minion's waypoint cursor if it reached the current one, and return the
/// world position it should walk toward now.
fn advance_lane(map: &MapData, registry: &mut Registry, id: EntityId, pos: Vec2) -> Option<Vec2> {
    let unit = registry.get_mut(id)?;
    let lane_idx = unit.lane?;
    let wps = &map.lanes.get(lane_idx)?.waypoints;
    let n = wps.len();
    if n == 0 {
        return None;
    }
    let forward = unit.forward;
    let goal_reached = if forward {
        unit.wp_index >= n - 1
    } else {
        unit.wp_index == 0
    };

    let current: Vec2 = wps[unit.wp_index.min(n - 1)].into();
    if pos.distance(current) <= WAYPOINT_TOLERANCE && !goal_reached {
        unit.wp_index = if forward {
            (unit.wp_index + 1).min(n - 1)
        } else {
            unit.wp_index.saturating_sub(1)
        };
    }
    Some(wps[unit.wp_index.min(n - 1)].into())
}
