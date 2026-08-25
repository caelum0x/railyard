//! Minion waves: spawn a composed wave for both teams on every lane on a fixed
//! cadence. Minions are sim champions that walk the lane path (see [`super::lanes`])
//! and fight through the shared combat core.

use serde::{Deserialize, Serialize};
use sim::{EntityId, Vec2, World, MAP_BOUNDS};

use super::map::{content_dir, load_ron, ContentError, MapData, UnitStats};
use super::{alloc_id, build_unit, MinionRole, Registry, Unit, UnitKind};

/// One minion type within a wave.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MinionDef {
    /// "melee" | "caster" | "siege".
    pub kind: String,
    pub stats: UnitStats,
    pub move_speed: f32,
    pub bounty: i32,
    pub count: u32,
}

impl MinionDef {
    fn role(&self) -> MinionRole {
        match self.kind.as_str() {
            "caster" => MinionRole::Caster,
            "siege" => MinionRole::Siege,
            _ => MinionRole::Melee,
        }
    }
}

/// The wave profile: when the first wave spawns, the interval between waves, and
/// the composition of one wave (per team, per lane).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WaveProfile {
    pub first_spawn: u64,
    pub interval: u64,
    pub minions: Vec<MinionDef>,
}

/// Load the wave profile from `content/minions/wave.ron`.
pub fn load_waves() -> Result<WaveProfile, ContentError> {
    load_ron(content_dir().join("minions").join("wave.ron"))
}

/// True if a wave is due to spawn at `tick`.
pub fn is_wave_tick(profile: &WaveProfile, tick: u64) -> bool {
    tick >= profile.first_spawn
        && profile.interval > 0
        && (tick - profile.first_spawn) % profile.interval == 0
}

/// If `tick` is a wave tick, spawn a full wave for both teams on every lane.
/// Returns the number of minions spawned.
pub fn maybe_spawn(
    profile: &WaveProfile,
    tick: u64,
    world: &mut World,
    registry: &mut Registry,
    next_id: &mut EntityId,
    map: &MapData,
) -> usize {
    if !is_wave_tick(profile, tick) {
        return 0;
    }
    let mut spawned = 0;
    for lane_idx in 0..map.lanes.len() {
        if map.lanes[lane_idx].waypoints.len() < 2 {
            continue;
        }
        spawned += spawn_column(world, registry, next_id, profile, map, lane_idx, 0, true);
        spawned += spawn_column(world, registry, next_id, profile, map, lane_idx, 1, false);
    }
    spawned
}

/// Spawn one team's minion column into `lane_idx`. `forward` is the team-0->team-1
/// path direction (team 1 walks it in reverse). Minions are staggered into a column
/// behind the entry point along the initial lane direction.
fn spawn_column(
    world: &mut World,
    registry: &mut Registry,
    next_id: &mut EntityId,
    profile: &WaveProfile,
    map: &MapData,
    lane_idx: usize,
    team: u8,
    forward: bool,
) -> usize {
    let wps = &map.lanes[lane_idx].waypoints;
    let n = wps.len();
    // Entry point + the first waypoint a minion heads toward.
    let (entry, first_wp): (Vec2, usize) = if forward {
        (wps[0].into(), 1)
    } else {
        (wps[n - 1].into(), n - 2)
    };
    let next: Vec2 = wps[first_wp].into();
    let dir = (next - entry).normalized(); // marching direction; column stacks behind

    let mut column_index = 0.0f32;
    let mut spawned = 0;
    for def in &profile.minions {
        let role = def.role();
        for _ in 0..def.count {
            let id = alloc_id(next_id);
            let pos = (entry - dir * (column_index * 1.2)).clamp_square(MAP_BOUNDS);
            let champ = build_unit(id, team, pos, &def.stats, def.move_speed);
            world.champions.push(champ);
            let mut unit =
                Unit::stationary(id, UnitKind::Minion(role), team, def.bounty, Some(lane_idx));
            unit.forward = forward;
            unit.wp_index = first_wp;
            registry.add(unit);
            column_index += 1.0;
            spawned += 1;
        }
    }
    spawned
}
