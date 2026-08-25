//! Match configuration and battler tunables.
//!
//! Everything here is compile-time-constant defaults plus an explicit, `Copy`
//! [`MatchConfig`]. Nothing reads the wall-clock or any global; a match is a pure
//! function of `(MatchConfig, decks, deploy decisions)`, which is what makes the
//! whole battler deterministic and reproducible from a seed.
//!
//! Lane / base geometry lives in [`super::lanes`]; the `MatchConfig` methods here
//! are thin delegations so call-sites keep the ergonomic `cfg.lane_x(..)` form.

use sim::Vec2;

use super::lanes;

/// Number of lanes cards can be deployed into. Lanes spread evenly across the width
/// of the play-plane; a unit spawns at its lane's x and marches toward the enemy base.
pub const DEFAULT_LANES: usize = 3;

/// Fixed ticks between an elixir regeneration event (one elixir added, per player).
/// At [`sim::TICK_HZ`] (30) this is one elixir every ~1.5s.
pub const DEFAULT_ELIXIR_REGEN_PERIOD: u64 = 45;

/// Elixir each player starts the match with.
pub const DEFAULT_START_ELIXIR: u32 = 5;

/// Hard cap on stored elixir (Clash-style pool).
pub const DEFAULT_MAX_ELIXIR: u32 = 10;

/// Distance of each base from the origin along the y-axis. Player 0's base sits at
/// `-BASE_Y`, player 1's at `+BASE_Y`. Kept inside [`sim::MAP_BOUNDS`].
pub const DEFAULT_BASE_Y: f32 = 40.0;

/// How far in front of its base (toward mid-field) a freshly deployed unit spawns.
pub const DEFAULT_SPAWN_OFFSET: f32 = 3.0;

/// Half-width of the lane spread: lane x-coordinates fall in `[-LANE_SPREAD, LANE_SPREAD]`.
pub const DEFAULT_LANE_SPREAD: f32 = 20.0;

// ---- Base / tower combat stats ---------------------------------------------
/// Base (tower) maximum health. The win condition is reducing this to 0.
pub const DEFAULT_BASE_HP: i32 = 2400;
/// Base auto-attack damage — a tower defends units that walk into its range.
pub const DEFAULT_BASE_AD: i32 = 45;
/// Base flat armor.
pub const DEFAULT_BASE_ARMOR: i32 = 20;
/// Base auto-attack range in world units.
pub const DEFAULT_BASE_RANGE: f32 = 12.0;
/// Ticks between base auto-attacks.
pub const DEFAULT_BASE_ATTACK_PERIOD: u32 = 24;

/// Immutable per-match rules. `Copy` so it is cheap to thread through and can never
/// be mutated mid-match (matches the immutability principle).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MatchConfig {
    pub seed: u64,
    pub lanes: usize,
    pub elixir_regen_period: u64,
    pub start_elixir: u32,
    pub max_elixir: u32,
    pub base_y: f32,
    pub spawn_offset: f32,
    pub lane_spread: f32,
    pub base_hp: i32,
    pub base_ad: i32,
    pub base_armor: i32,
    pub base_range: f32,
    pub base_attack_period: u32,
    /// Hard tick cap; a match that reaches it with both bases alive is a draw.
    pub max_ticks: u64,
}

impl Default for MatchConfig {
    fn default() -> Self {
        MatchConfig {
            seed: 42,
            lanes: DEFAULT_LANES,
            elixir_regen_period: DEFAULT_ELIXIR_REGEN_PERIOD,
            start_elixir: DEFAULT_START_ELIXIR,
            max_elixir: DEFAULT_MAX_ELIXIR,
            base_y: DEFAULT_BASE_Y,
            spawn_offset: DEFAULT_SPAWN_OFFSET,
            lane_spread: DEFAULT_LANE_SPREAD,
            base_hp: DEFAULT_BASE_HP,
            base_ad: DEFAULT_BASE_AD,
            base_armor: DEFAULT_BASE_ARMOR,
            base_range: DEFAULT_BASE_RANGE,
            base_attack_period: DEFAULT_BASE_ATTACK_PERIOD,
            max_ticks: 20_000,
        }
    }
}

impl MatchConfig {
    /// World-space position of a player's base (tower). See [`lanes::base_pos`].
    pub fn base_pos(&self, player: u8) -> Vec2 {
        lanes::base_pos(self, player)
    }

    /// The x-coordinate of `lane` (0-based). See [`lanes::lane_x`].
    pub fn lane_x(&self, lane: usize) -> f32 {
        lanes::lane_x(self, lane)
    }

    /// Spawn position for a unit deployed by `player` into `lane`. See
    /// [`lanes::spawn_pos`].
    pub fn spawn_pos(&self, player: u8, lane: usize) -> Vec2 {
        lanes::spawn_pos(self, player, lane)
    }
}
