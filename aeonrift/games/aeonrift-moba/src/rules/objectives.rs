//! Neutral epic objectives + the shared neutral catalog.
//!
//! Objectives (and jungle camps, see [`super::jungle`]) are team-255 sim champions
//! that stand still and fight anything in range. On death they pay a bounty and, for
//! epics, apply a permanent team-wide buff to the killing team. The catalog is the
//! stat/reward source; maps only supply positions + ids.

use serde::{Deserialize, Serialize};
use sim::{EntityId, World};

use super::map::{content_dir, load_ron, ContentError, MapData, UnitStats};
use super::{alloc_id, build_unit, Registry, Unit, UnitKind, NEUTRAL};

/// A team-wide reward applied when an epic objective dies.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeutralBuff {
    /// Flat attack damage added to every living champion on the killing team.
    #[serde(default)]
    pub bonus_ad: i32,
    /// Flat health restored to every living champion on the killing team.
    #[serde(default)]
    pub team_heal: i32,
}

/// One neutral entry (a jungle camp or an epic objective).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeutralDef {
    pub id: String,
    pub stats: UnitStats,
    pub bounty: i32,
    #[serde(default)]
    pub buff: Option<NeutralBuff>,
}

/// The neutral catalog loaded from `content/objectives/neutrals.ron`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NeutralCatalog {
    pub entries: Vec<NeutralDef>,
}

impl NeutralCatalog {
    /// Look up a neutral by id.
    pub fn get(&self, id: &str) -> Option<&NeutralDef> {
        self.entries.iter().find(|e| e.id == id)
    }
}

/// Load the neutral catalog.
pub fn load_neutrals() -> Result<NeutralCatalog, ContentError> {
    load_ron(content_dir().join("objectives").join("neutrals.ron"))
}

/// Spawn every epic objective the map lists (resolving stats from the catalog).
/// Unknown ids are skipped. Returns the number spawned.
pub fn spawn(
    world: &mut World,
    registry: &mut Registry,
    next_id: &mut EntityId,
    map: &MapData,
    catalog: &NeutralCatalog,
) -> usize {
    let mut spawned = 0;
    for obj in &map.objectives {
        let Some(def) = catalog.get(&obj.id) else {
            continue;
        };
        let id = alloc_id(next_id);
        let champ = build_unit(id, NEUTRAL, obj.pos.into(), &def.stats, 0.0);
        world.champions.push(champ);
        let mut unit = Unit::stationary(id, UnitKind::Objective, NEUTRAL, def.bounty, None);
        unit.neutral_id = def.id.clone();
        registry.add(unit);
        spawned += 1;
    }
    spawned
}

/// Apply a neutral buff to every living champion on `team`.
pub fn apply_team_buff(world: &mut World, registry: &Registry, team: u8, buff: &NeutralBuff) {
    let champ_ids: Vec<EntityId> = registry
        .iter()
        .filter(|u| u.kind == UnitKind::Champion && u.team == team)
        .map(|u| u.id)
        .collect();
    for id in champ_ids {
        if let Some(c) = world.champion_mut(id) {
            if !c.alive {
                continue;
            }
            if buff.bonus_ad != 0 {
                c.attack_damage += buff.bonus_ad;
            }
            if buff.team_heal > 0 {
                let healed = (c.health.current + buff.team_heal).min(c.health.max);
                c.health = sim::Health {
                    current: healed,
                    max: c.health.max,
                };
            }
        }
    }
}
