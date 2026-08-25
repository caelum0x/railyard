//! MOBA rules layer.
//!
//! Every combat entity in a match — champion, minion, turret, inhibitor, nexus,
//! jungle monster, epic objective — is stored in the sim as a [`sim::Champion`], so
//! they *all* fight through the one deterministic combat core (auto-attacks, armor
//! mitigation, projectiles, death). This module adds the MOBA-specific meaning the
//! sim does not carry: what an entity IS, which lane it belongs to, what it's worth,
//! and the topology it lives on.
//!
//! Submodules:
//! - [`map`]        — RON map + content loading,
//! - [`waves`]      — minion wave spawning on a fixed cadence,
//! - [`lanes`]      — deterministic minion lane-follow + engage AI,
//! - [`turrets`] / [`inhibitors`] / [`nexus`] — structures + the win condition,
//! - [`jungle`] / [`objectives`] — neutral camps + epic buffs,
//! - [`vision`]     — team fog-of-war queries,
//! - [`shop`]       — item purchasing.

pub mod inhibitors;
pub mod jungle;
pub mod lanes;
pub mod map;
pub mod nexus;
pub mod objectives;
pub mod shop;
pub mod turrets;
pub mod vision;
pub mod waves;

use sim::{Champion, EntityId, TICK_HZ, Vec2};

use map::UnitStats;

/// Team ids. Two player teams plus a neutral team for jungle/objective units.
pub const BLUE: u8 = 0;
pub const RED: u8 = 1;
pub const NEUTRAL: u8 = 255;

/// The opposing player team (Blue <-> Red). Neutral maps to Blue by convention
/// (only used where a definite enemy team is required and never for neutrals).
pub fn enemy_of(team: u8) -> u8 {
    match team {
        BLUE => RED,
        RED => BLUE,
        _ => BLUE,
    }
}

/// What a registered entity actually is. The sim only sees a `Champion`; this is
/// the MOBA classification kept alongside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitKind {
    /// A player- or bot-controlled champion.
    Champion,
    /// A lane minion (melee / caster / siege — captured in `MinionRole`).
    Minion(MinionRole),
    /// A lane turret.
    Turret,
    /// An inhibitor (its fall exposes the owning team's nexus).
    Inhibitor,
    /// The nexus — destroy the enemy's to win.
    Nexus,
    /// A jungle camp monster.
    JungleCamp,
    /// An epic neutral objective (grants a team-wide buff on death).
    Objective,
}

/// Minion role, used only for wave composition + bounty; combat is stat-driven.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MinionRole {
    Melee,
    Caster,
    Siege,
}

/// The MOBA metadata for one entity, kept in id order alongside the sim world.
///
/// For minions, `lane` + `wp_index` + `forward` track lane-follow progress; other
/// kinds ignore them. `neutral_id` names a jungle/objective entry so its reward can
/// be looked up on death.
#[derive(Clone, Debug, PartialEq)]
pub struct Unit {
    pub id: EntityId,
    pub kind: UnitKind,
    pub team: u8,
    pub bounty: i32,
    /// Lane index into `MapData.lanes`, for minions/turrets/inhibitors.
    pub lane: Option<usize>,
    /// Next waypoint index along the lane path (minions only).
    pub wp_index: usize,
    /// Traversal direction along the lane path: true = team0->team1 order.
    pub forward: bool,
    /// Catalog id for a neutral camp/objective (empty otherwise).
    pub neutral_id: String,
}

impl Unit {
    /// A structure/champion unit that does not follow a lane path.
    pub fn stationary(id: EntityId, kind: UnitKind, team: u8, bounty: i32, lane: Option<usize>) -> Self {
        Unit {
            id,
            kind,
            team,
            bounty,
            lane,
            wp_index: 0,
            forward: true,
            neutral_id: String::new(),
        }
    }
}

/// The entity registry: MOBA metadata kept in ascending id order to mirror the
/// sim's champion vec (deterministic iteration — never a hash map).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Registry {
    units: Vec<Unit>,
}

impl Registry {
    pub fn new() -> Self {
        Registry { units: Vec::new() }
    }

    /// Register a unit. Units must be added in ascending id order (the caller
    /// allocates ids monotonically) so the vec stays sorted for binary lookup.
    pub fn add(&mut self, unit: Unit) {
        debug_assert!(
            self.units.last().map(|u| u.id < unit.id).unwrap_or(true),
            "units must be registered in ascending id order"
        );
        self.units.push(unit);
    }

    /// Immutable lookup by id (binary search on the sorted vec).
    pub fn get(&self, id: EntityId) -> Option<&Unit> {
        self.units
            .binary_search_by_key(&id, |u| u.id)
            .ok()
            .map(|i| &self.units[i])
    }

    /// Mutable lookup by id.
    pub fn get_mut(&mut self, id: EntityId) -> Option<&mut Unit> {
        match self.units.binary_search_by_key(&id, |u| u.id) {
            Ok(i) => Some(&mut self.units[i]),
            Err(_) => None,
        }
    }

    /// All units in id order.
    pub fn iter(&self) -> impl Iterator<Item = &Unit> {
        self.units.iter()
    }

    /// All units in id order (mutable).
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut Unit> {
        self.units.iter_mut()
    }

    /// Number of registered units.
    pub fn len(&self) -> usize {
        self.units.len()
    }

    /// True if no units are registered.
    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }
}

/// Allocate the next monotonic entity id (keeps the sim's champion vec + the
/// registry sorted by construction: everything is appended in increasing id order).
pub fn alloc_id(next_id: &mut EntityId) -> EntityId {
    let id = *next_id;
    *next_id += 1;
    id
}

/// Compile [`UnitStats`] into a concrete sim champion at `pos` on `team`.
///
/// `attack_speed` (attacks/second) becomes an integer tick period
/// `round(TICK_HZ / attack_speed)` (min 1) so combat stays on the sim's exact
/// integer cooldown clock. `move_speed` is the walk speed (0 for structures).
pub fn build_unit(
    id: EntityId,
    team: u8,
    pos: Vec2,
    stats: &UnitStats,
    move_speed: f32,
) -> Champion {
    let period = ((TICK_HZ as f32) / stats.attack_speed.max(0.01))
        .round()
        .max(1.0) as u32;
    Champion::new(id, team, pos)
        .with_max_health(stats.hp)
        .with_attack_damage(stats.ad)
        .with_armor(stats.armor)
        .with_attack_range(stats.range)
        .with_attack_period(period)
        .with_move_speed(move_speed)
}
