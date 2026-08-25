//! Jungle camps: neutral monsters that hold a position and reward the team that
//! kills them (gold, and possibly a heal). They reuse the neutral catalog + the same
//! sim-champion representation as epic objectives (see [`super::objectives`]).

use sim::{EntityId, World};

use super::map::MapData;
use super::objectives::NeutralCatalog;
use super::{alloc_id, build_unit, Registry, Unit, UnitKind, NEUTRAL};

/// Spawn every jungle camp the map lists (stats/rewards from the catalog). Unknown
/// ids are skipped. Returns the number spawned.
pub fn spawn(
    world: &mut World,
    registry: &mut Registry,
    next_id: &mut EntityId,
    map: &MapData,
    catalog: &NeutralCatalog,
) -> usize {
    let mut spawned = 0;
    for camp in &map.jungle {
        let Some(def) = catalog.get(&camp.id) else {
            continue;
        };
        let id = alloc_id(next_id);
        let champ = build_unit(id, NEUTRAL, camp.pos.into(), &def.stats, 0.0);
        world.champions.push(champ);
        let mut unit = Unit::stationary(id, UnitKind::JungleCamp, NEUTRAL, def.bounty, None);
        unit.neutral_id = def.id.clone();
        registry.add(unit);
        spawned += 1;
    }
    spawned
}

/// How many jungle camps are still alive.
pub fn alive_count(world: &World, registry: &Registry) -> usize {
    registry
        .iter()
        .filter(|u| u.kind == UnitKind::JungleCamp)
        .filter(|u| world.champion(u.id).map(|c| c.alive).unwrap_or(false))
        .count()
}
