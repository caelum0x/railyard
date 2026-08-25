//! Inhibitors: structures whose destruction (together with the lane turrets)
//! exposes the owning team's nexus. They never attack (ad 0, range 0) — they only
//! stand and take damage, again reusing the sim champion representation.

use sim::{EntityId, World};

use super::map::{MapData, StructureStats};
use super::{alloc_id, build_unit, Registry, Unit, UnitKind};

/// Gold awarded to the enemy team for destroying an inhibitor.
pub const INHIBITOR_BOUNTY: i32 = 50;

/// Spawn every inhibitor defined across the map's lanes.
pub fn spawn(
    world: &mut World,
    registry: &mut Registry,
    next_id: &mut EntityId,
    map: &MapData,
    stats: &StructureStats,
) {
    for (lane_idx, lane) in map.lanes.iter().enumerate() {
        for inh in &lane.inhibitors {
            let id = alloc_id(next_id);
            let champ = build_unit(id, inh.team, inh.pos.into(), &stats.inhibitor, 0.0);
            world.champions.push(champ);
            registry.add(Unit::stationary(
                id,
                UnitKind::Inhibitor,
                inh.team,
                INHIBITOR_BOUNTY,
                Some(lane_idx),
            ));
        }
    }
}

/// How many inhibitors belonging to `team` are still alive.
pub fn alive_count(world: &World, registry: &Registry, team: u8) -> usize {
    registry
        .iter()
        .filter(|u| u.kind == UnitKind::Inhibitor && u.team == team)
        .filter(|u| world.champion(u.id).map(|c| c.alive).unwrap_or(false))
        .count()
}
