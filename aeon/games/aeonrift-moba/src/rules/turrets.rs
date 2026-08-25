//! Turrets: stationary structures that auto-attack enemies in a long range.
//!
//! A turret is just a [`sim::Champion`] with no move target and a big attack range,
//! so it fights through the same combat core as everything else — no bespoke turret
//! combat code. Spawning reads slots + teams from the map and stats from the
//! structure catalog.

use sim::{EntityId, World};

use super::map::{MapData, StructureStats};
use super::{alloc_id, build_unit, Registry, Unit, UnitKind};

/// Gold awarded to the enemy team for destroying a turret.
pub const TURRET_BOUNTY: i32 = 250;

/// Spawn every turret defined across the map's lanes.
pub fn spawn(
    world: &mut World,
    registry: &mut Registry,
    next_id: &mut EntityId,
    map: &MapData,
    stats: &StructureStats,
) {
    for (lane_idx, lane) in map.lanes.iter().enumerate() {
        for t in &lane.turrets {
            let id = alloc_id(next_id);
            // move_speed 0.0: turrets never path.
            let champ = build_unit(id, t.team, t.pos.into(), &stats.turret, 0.0);
            world.champions.push(champ);
            registry.add(Unit::stationary(
                id,
                UnitKind::Turret,
                t.team,
                TURRET_BOUNTY,
                Some(lane_idx),
            ));
        }
    }
}

/// How many turrets belonging to `team` are still alive.
pub fn alive_count(world: &World, registry: &Registry, team: u8) -> usize {
    registry
        .iter()
        .filter(|u| u.kind == UnitKind::Turret && u.team == team)
        .filter(|u| world.champion(u.id).map(|c| c.alive).unwrap_or(false))
        .count()
}
