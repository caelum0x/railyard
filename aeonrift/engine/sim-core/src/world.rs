//! The simulation world: a tiny, deterministic, ECS-style container.
//!
//! Storage is a `Vec` iterated in stable id order (never a `HashMap`) so system
//! execution order is deterministic. All tunables are compile-time constants.
//!
//! ## M2 combat core
//! Champions carry combat stats (health, attack damage, armor, range, cooldowns)
//! and a `team`. The world also holds a `Vec<Projectile>` for skillshot abilities
//! and a monotonic `next_projectile_id`. Every field is hashed by
//! [`crate::hash::state_hash`], so the determinism gate covers combat too.

use crate::input::EntityId;
use crate::math::Vec2;
use crate::rng::Rng;

/// Fixed simulation rate.
pub const TICK_HZ: u32 = 30;
/// Fixed timestep in seconds. No wall-clock is ever read in the sim.
pub const DT: f32 = 1.0 / TICK_HZ as f32;
/// Champion movement speed, units per second.
pub const MOVE_SPEED: f32 = 10.0;
/// Distance at which a champion is considered "arrived" and snaps to the target.
pub const ARRIVE_TOLERANCE: f32 = 0.01;
/// Half-extent of the square play-plane. Authoritative input validation clamps
/// move targets into `[-MAP_BOUNDS, MAP_BOUNDS]` on each axis.
pub const MAP_BOUNDS: f32 = 50.0;
/// Radius around the origin where champions spawn (RNG-seeded).
pub const SPAWN_RADIUS: f32 = 5.0;

// ---- Combat tunables (defaults for `Champion::new`) ------------------------
/// Default maximum health for a freshly spawned champion.
pub const DEFAULT_MAX_HP: i32 = 600;
/// Default auto-attack damage (pre-mitigation).
pub const DEFAULT_ATTACK_DAMAGE: i32 = 60;
/// Default flat armor. Mitigation: `effective = raw * 100 / (100 + armor)`.
pub const DEFAULT_ARMOR: i32 = 30;
/// Default auto-attack range, in world units.
pub const DEFAULT_ATTACK_RANGE: f32 = 8.0;
/// Default ticks between auto-attacks (30 ticks = 1 attack / second at 30 Hz).
pub const DEFAULT_ATTACK_PERIOD: u32 = 30;

// ---- Ability / projectile tunables -----------------------------------------
/// Skillshot damage (pre-mitigation) applied on hit.
pub const ABILITY_DAMAGE: i32 = 120;
/// Ticks the ability is on cooldown after a successful cast.
pub const ABILITY_COOLDOWN_TICKS: u32 = 90;
/// Maximum distance a skillshot projectile travels before expiring (a miss).
pub const ABILITY_RANGE: f32 = 30.0;
/// Projectile travel speed, world units per second.
pub const PROJECTILE_SPEED: f32 = 40.0;
/// Collision radius: a projectile hits an enemy within this distance.
pub const PROJECTILE_HIT_RADIUS: f32 = 1.5;

// ---- Status-effect tunables (M5 DSL effect vocabulary) ---------------------
/// Movement multiplier applied while a champion is slowed. A slowed champion
/// moves at `move_speed * SLOW_MOVE_FACTOR`. Integer-free but bit-reproducible.
pub const SLOW_MOVE_FACTOR: f32 = 0.5;

/// Health pool. Integer HP keeps combat math exact and deterministic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Health {
    pub current: i32,
    pub max: i32,
}

impl Health {
    /// A full health pool with the given maximum.
    pub const fn new(max: i32) -> Self {
        Health { current: max, max }
    }

    /// True once health has been depleted.
    pub const fn is_dead(&self) -> bool {
        self.current <= 0
    }

    /// Return a NEW `Health` with `amount` subtracted (immutable — never mutates
    /// in place). `amount` is the already-mitigated damage.
    pub const fn damaged(self, amount: i32) -> Health {
        Health {
            current: self.current - amount,
            max: self.max,
        }
    }
}

/// A controllable champion entity with combat stats.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Champion {
    pub id: EntityId,
    /// Team id. Auto-attacks and skillshots only affect *other* teams.
    pub team: u8,
    pub pos: Vec2,
    /// Current click-to-move destination, if any. `None` == idle/arrived.
    pub target: Option<Vec2>,
    pub health: Health,
    /// Auto-attack damage before armor mitigation.
    pub attack_damage: i32,
    /// Flat armor used by the mitigation formula.
    pub armor: i32,
    /// Auto-attack range in world units.
    pub attack_range: f32,
    /// Ticks between auto-attacks.
    pub attack_period: u32,
    /// Ticks remaining until the next auto-attack is available (0 == ready).
    pub attack_cooldown: u32,
    /// Ticks remaining until the ability is available (0 == ready).
    pub ability_cooldown: u32,
    /// Per-champion movement speed, units per second. Defaults to [`MOVE_SPEED`]
    /// so M0/M1 scenarios are unchanged; DSL champions set their own.
    pub move_speed: f32,
    /// Ticks remaining that the champion is STUNNED (0 == not stunned). A stunned
    /// champion cannot move, auto-attack, or cast until it expires.
    pub stun_ticks: u32,
    /// Ticks remaining that the champion is SLOWED (0 == not slowed). A slowed
    /// champion moves at `move_speed * SLOW_MOVE_FACTOR`.
    pub slow_ticks: u32,
    /// Dead champions are inert: they do not move, attack, cast, or take damage.
    pub alive: bool,
}

impl Champion {
    /// A champion with default combat stats at `pos` on `team`.
    pub fn new(id: EntityId, team: u8, pos: Vec2) -> Self {
        Champion {
            id,
            team,
            pos,
            target: None,
            health: Health::new(DEFAULT_MAX_HP),
            attack_damage: DEFAULT_ATTACK_DAMAGE,
            armor: DEFAULT_ARMOR,
            attack_range: DEFAULT_ATTACK_RANGE,
            attack_period: DEFAULT_ATTACK_PERIOD,
            attack_cooldown: 0,
            ability_cooldown: 0,
            move_speed: MOVE_SPEED,
            stun_ticks: 0,
            slow_ticks: 0,
            alive: true,
        }
    }

    // Builder-style setters return a new value (no external mutation), used to
    // configure duel/unit-test champions with explicit stats.
    pub fn with_max_health(mut self, max: i32) -> Self {
        self.health = Health::new(max);
        self
    }
    pub fn with_attack_damage(mut self, ad: i32) -> Self {
        self.attack_damage = ad;
        self
    }
    pub fn with_armor(mut self, armor: i32) -> Self {
        self.armor = armor;
        self
    }
    pub fn with_attack_range(mut self, range: f32) -> Self {
        self.attack_range = range;
        self
    }
    pub fn with_attack_period(mut self, period: u32) -> Self {
        self.attack_period = period;
        self
    }
    pub fn with_move_speed(mut self, speed: f32) -> Self {
        self.move_speed = speed;
        self
    }
}

/// A moving skillshot projectile. Deterministic: fixed velocity, fixed-step
/// integration, collision resolved against champions in stable id order.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Projectile {
    pub id: EntityId,
    /// Champion that fired it (for attribution / debugging).
    pub owner: EntityId,
    /// Team of the owner; the projectile only hits *other* teams.
    pub team: u8,
    pub pos: Vec2,
    /// Velocity in world units per second (displacement per tick is `vel * DT`).
    pub vel: Vec2,
    /// Damage applied on hit (before armor mitigation).
    pub damage: i32,
    /// Remaining distance the projectile may travel before it expires (a miss).
    pub remaining_range: f32,
    /// Collision radius against enemy champions.
    pub hit_radius: f32,
    /// Ticks of STUN applied to the champion this projectile hits (0 == none).
    /// Lets a DSL skillshot carry crowd control, not just damage.
    pub stun_on_hit: u32,
    /// Ticks of SLOW applied to the champion this projectile hits (0 == none).
    pub slow_on_hit: u32,
}

/// The whole simulation state. `Clone` + `PartialEq` power snapshots and the
/// determinism gate's full-state equality check.
#[derive(Clone, Debug, PartialEq)]
pub struct World {
    pub tick: u64,
    pub rng: Rng,
    /// Champions, kept sorted by id for deterministic iteration.
    pub champions: Vec<Champion>,
    /// In-flight projectiles, kept in ascending id order.
    pub projectiles: Vec<Projectile>,
    /// Monotonic id source for spawned projectiles (hashed, so any drift shows).
    pub next_projectile_id: EntityId,
}

impl World {
    /// Create a world from a seed and spawn one champion at an RNG-derived
    /// position (so the seed meaningfully affects the state hash). This preserves
    /// the M0/M1 single-champion scenario used by the movement/netcode gates.
    pub fn new(seed: u64) -> Self {
        let mut rng = Rng::new(seed);
        let spawn = Vec2::new(
            rng.range_f32(-SPAWN_RADIUS, SPAWN_RADIUS),
            rng.range_f32(-SPAWN_RADIUS, SPAWN_RADIUS),
        );
        let champions = vec![Champion::new(0, 0, spawn)];
        World {
            tick: 0,
            rng,
            champions,
            projectiles: Vec::new(),
            next_projectile_id: 0,
        }
    }

    /// Create a world with an explicit roster of champions (M2 combat scenarios:
    /// duels, ability tests). Champions must be supplied in ascending id order to
    /// preserve deterministic iteration.
    pub fn from_champions(seed: u64, champions: Vec<Champion>) -> Self {
        World {
            tick: 0,
            rng: Rng::new(seed),
            champions,
            projectiles: Vec::new(),
            next_projectile_id: 0,
        }
    }

    /// Immutable lookup by id.
    pub fn champion(&self, id: EntityId) -> Option<&Champion> {
        self.champions.iter().find(|c| c.id == id)
    }

    /// Mutable lookup by id.
    pub fn champion_mut(&mut self, id: EntityId) -> Option<&mut Champion> {
        self.champions.iter_mut().find(|c| c.id == id)
    }
}
