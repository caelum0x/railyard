//! `sim` — the deterministic, headless, engine-agnostic MOBA simulation core.
//!
//! This crate is compiled into both the authoritative `server` and the predicting
//! `client`. Its public surface is intentionally tiny and engine-neutral:
//!
//! - [`World::new`] to create a seeded world,
//! - [`step`] to advance the world one fixed tick given a slice of inputs,
//! - [`state_hash`] to fingerprint the full state.
//!
//! ## Determinism contract
//! - Fixed timestep ([`world::DT`]), never the wall-clock.
//! - Seeded RNG ([`rng::Rng`], SplitMix64); RNG state is hashed.
//! - Stable, ordered iteration (`Vec` in id order; inputs applied in caller order).
//! - Floats hashed by raw bits; on a fixed machine `f32` math is bit-reproducible.
//!
//! Same seed + same input sequence => byte-identical state and hash across runs.

pub mod hash;
pub mod input;
pub mod math;
pub mod net;
pub mod rng;
pub mod systems;
pub mod world;

pub use hash::state_hash;
pub use input::{Command, EntityId, Input};
pub use math::Vec2;
pub use net::{Snapshot, StampedInput};
pub use rng::Rng;
pub use systems::{
    apply_ability_damage, apply_dash, apply_heal, apply_slow, apply_stun, mitigated_damage,
    spawn_projectile,
};
pub use world::{
    Champion, Health, Projectile, World, ABILITY_COOLDOWN_TICKS, ABILITY_DAMAGE, ABILITY_RANGE, DT,
    MAP_BOUNDS, MOVE_SPEED, PROJECTILE_HIT_RADIUS, PROJECTILE_SPEED, SLOW_MOVE_FACTOR, TICK_HZ,
};

/// Advance the world by exactly one fixed tick.
///
/// Deterministic system order:
/// 1. apply inputs (move targets, and ability casts that spawn projectiles),
/// 2. integrate champion movement,
/// 3. tick cooldowns down,
/// 4. advance projectiles + resolve their hits,
/// 5. resolve auto-attacks,
/// 6. bump the tick counter.
///
/// Death is handled inline where damage is applied (systems::apply_damage): a
/// champion at <= 0 HP is marked dead and every system skips it thereafter.
/// `inputs` must already be in a deterministic order (e.g. sorted by sequence).
pub fn step(world: &mut World, inputs: &[Input]) {
    systems::apply_inputs(world, inputs);
    systems::movement_system(world);
    systems::cooldown_system(world);
    systems::projectile_system(world);
    systems::auto_attack_system(world);
    world.tick += 1;
}
