//! The nexus + the win condition.
//!
//! Each team has one nexus. Destroying the enemy nexus wins the match. A nexus is
//! *protected* until its team's turrets and inhibitors are all destroyed; while
//! protected it wears a near-infinite armor (from the structure catalog) so the
//! sim's real armor formula mitigates all incoming damage to ~0. When the last
//! protector falls, [`expose_ready`] drops the armor to its base value and the
//! nexus becomes killable — no bespoke shield/invulnerability type needed.

use sim::{EntityId, World};

use super::map::{MapData, StructureStats};
use super::{alloc_id, build_unit, enemy_of, inhibitors, turrets, Registry, Unit, UnitKind};

/// Spawn both teams' nexuses, each starting with the protective shield armor.
pub fn spawn(
    world: &mut World,
    registry: &mut Registry,
    next_id: &mut EntityId,
    map: &MapData,
    stats: &StructureStats,
) {
    for base in &map.bases {
        let id = alloc_id(next_id);
        let mut champ = build_unit(id, base.team, base.nexus.into(), &stats.nexus, 0.0);
        // Protected: near-infinite armor until exposed.
        champ.armor = stats.nexus_shield_armor;
        world.champions.push(champ);
        registry.add(Unit::stationary(id, UnitKind::Nexus, base.team, 0, None));
    }
}

/// Is `team`'s nexus currently protected (turrets or inhibitors still standing)?
pub fn is_protected(world: &World, registry: &Registry, team: u8) -> bool {
    turrets::alive_count(world, registry, team) > 0
        || inhibitors::alive_count(world, registry, team) > 0
}

/// Drop the protective armor from any nexus whose protectors are all gone. Runs
/// each tick; idempotent (only lowers a still-shielded nexus once exposed).
pub fn expose_ready(world: &mut World, registry: &Registry, stats: &StructureStats) {
    // Collect exposable nexus ids first (immutable borrow), then mutate.
    let to_expose: Vec<EntityId> = registry
        .iter()
        .filter(|u| u.kind == UnitKind::Nexus)
        .filter(|u| !is_protected(world, registry, u.team))
        .map(|u| u.id)
        .collect();

    for id in to_expose {
        if let Some(c) = world.champion_mut(id) {
            if c.armor > stats.nexus.armor {
                c.armor = stats.nexus.armor;
            }
        }
    }
}

/// The winning team, if a nexus has fallen: whoever owns the *other* nexus.
pub fn winner(world: &World, registry: &Registry) -> Option<u8> {
    for u in registry.iter() {
        if u.kind == UnitKind::Nexus {
            if let Some(c) = world.champion(u.id) {
                if !c.alive {
                    return Some(enemy_of(u.team));
                }
            }
        }
    }
    None
}
