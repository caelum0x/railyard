//! Stylized ability-cast VFX: a faction-colored particle burst per skillshot.
//!
//! The seam is the sim itself: when a deployed unit casts its skillshot (see
//! [`super::tick::cast_inputs`]) the core spawns a real `sim::Projectile`. This module
//! WATCHES the world's projectile list and, the moment a new projectile id appears,
//! spawns one clear radial burst at the caster — colored by the champion's faction /
//! team ([`super::game::vfx_color`]) and driven bright enough that the camera's HDR
//! bloom makes it glow. It is a pure view: it never touches match state, only reacts to
//! the deterministic core's cast events.
//!
//! Tech note: this is a lightweight, dependency-free sprite burst (each shard is a
//! `Sprite::from_color` quad) rather than `bevy_hanabi` GPU particles — Bevy 0.19 is
//! bleeding-edge and hanabi has no matching release, so a hand-rolled burst is the
//! clean, build-green choice. The shape/shard count is fixed so every cast reads the
//! same way.

use std::collections::HashSet;

use bevy::prelude::*;

use super::game::vfx_color;
use super::tick::TICK_HZ;
use sim::EntityId;

/// Number of outward shards in a cast burst (a readable radial star).
const SHARDS: usize = 12;
/// Seconds a shard lives before it is despawned.
const SHARD_TTL: f32 = 0.5;
/// Seconds the central flash lives.
const FLASH_TTL: f32 = 0.2;
/// Outward shard speed, world units per second.
const SHARD_SPEED: f32 = 20.0;
/// How far from the caster each shard is born (so the star reads as a ring, not a
/// square that hides the champion's face).
const SHARD_INSET: f32 = 1.8;

/// Remembers which projectile ids have already triggered a burst, so each cast fires
/// its VFX exactly once. Projectile ids are monotonic and never reused, so this only
/// grows with the number of casts in a match (bounded, cheap).
#[derive(Resource, Default)]
pub struct CastTracker {
    seen: HashSet<EntityId>,
}

/// One animated burst particle: a flash or a shard. Advances outward, fades, and is
/// despawned when its life runs out.
#[derive(Component)]
pub struct VfxShard {
    /// World-space velocity (0 for the central flash).
    velocity: Vec2,
    /// Seconds of life remaining.
    ttl: f32,
    /// Original lifetime, for the fade curve.
    max_ttl: f32,
    /// The base HDR color (linear, may exceed 1.0 so bloom glows).
    base: LinearRgba,
    /// Size at birth (shrinks toward zero as it fades).
    birth_size: Vec2,
}

/// Detect newly cast skillshots and spawn a burst for each. Reads the world's live
/// projectile list against [`CastTracker`]; anchors the burst on the casting champion
/// (its current position) so the flash reads as coming from the unit, not the missile.
pub fn spawn_cast_vfx(
    mut commands: Commands,
    battle: Res<super::game::Battle>,
    mut tracker: ResMut<CastTracker>,
) {
    for proj in &battle.state.world.projectiles {
        if tracker.seen.contains(&proj.id) {
            continue;
        }
        tracker.seen.insert(proj.id);

        // Anchor on the caster if still alive (a cast flash belongs on the unit),
        // else fall back to where the projectile was born.
        let caster = battle.state.world.champion(proj.owner);
        let origin = caster.map(|c| c.pos).unwrap_or(proj.pos);
        let slug = battle.slug_of(proj.owner).unwrap_or("unknown");
        let color = vfx_color(proj.team, slug).to_linear();
        spawn_burst(&mut commands, Vec2::new(origin.x, origin.y), color);
    }
}

/// Spawn one radial burst (a bright central flash + [`SHARDS`] outward shards) at
/// `pos`, all in `color`. Shard directions are fixed angles so every cast looks alike.
fn spawn_burst(commands: &mut Commands, pos: Vec2, color: LinearRgba) {
    // Central core: a small bright diamond that blooms into a glow, then fades fast. A
    // touch of alpha lets the champion's face read through instead of being covered.
    let core = LinearRgba::new(color.red, color.green, color.blue, 0.85);
    let core_size = Vec2::splat(1.9);
    commands.spawn((
        Sprite::from_color(Color::from(core), core_size),
        Transform {
            translation: Vec3::new(pos.x, pos.y, 1.2),
            rotation: Quat::from_rotation_z(std::f32::consts::FRAC_PI_4), // diamond
            ..default()
        },
        VfxShard {
            velocity: Vec2::ZERO,
            ttl: FLASH_TTL,
            max_ttl: FLASH_TTL,
            base: core,
            birth_size: core_size,
        },
    ));

    // Radial shards: elongated quads born just off the caster and streaking outward, so
    // the burst reads as one clear radial star per cast.
    for i in 0..SHARDS {
        let ang = (i as f32) / (SHARDS as f32) * std::f32::consts::TAU;
        let dir = Vec2::new(ang.cos(), ang.sin());
        let size = Vec2::new(3.2, 0.55);
        let start = pos + dir * SHARD_INSET;
        commands.spawn((
            Sprite::from_color(Color::from(color), size),
            Transform {
                translation: Vec3::new(start.x, start.y, 1.1),
                rotation: Quat::from_rotation_z(ang),
                ..default()
            },
            VfxShard {
                velocity: dir * SHARD_SPEED,
                ttl: SHARD_TTL,
                max_ttl: SHARD_TTL,
                base: color,
                birth_size: size,
            },
        ));
    }
}

/// Advance every burst particle: drift outward, fade the HDR color and shrink toward
/// zero, and despawn once spent. Uses the fixed tick dt so it also animates cleanly in
/// the headless offscreen renderer (which advances time by hand).
pub fn animate_vfx(
    mut commands: Commands,
    time: Res<Time>,
    mut shards: Query<(Entity, &mut VfxShard, &mut Transform, &mut Sprite)>,
) {
    // Fall back to the fixed timestep if the app has no wall-clock delta yet, and cap
    // the step so a slow/first frame (e.g. texture loads) can't nuke a burst instantly.
    let mut dt = time.delta_secs();
    if dt <= 0.0 {
        dt = (1.0 / TICK_HZ) as f32;
    }
    dt = dt.min(0.05);
    for (entity, mut shard, mut tf, mut sprite) in &mut shards {
        shard.ttl -= dt;
        if shard.ttl <= 0.0 {
            commands.entity(entity).despawn();
            continue;
        }
        let frac = (shard.ttl / shard.max_ttl).clamp(0.0, 1.0);
        // Drift outward.
        let v = shard.velocity * dt;
        tf.translation.x += v.x;
        tf.translation.y += v.y;
        // Fade the HDR color toward black (keeps bloom bright early, gone at the end).
        let b = shard.base;
        sprite.color =
            Color::linear_rgba(b.red * frac, b.green * frac, b.blue * frac, b.alpha * frac);
        // Shrink so the burst reads as dissipating energy.
        let s = 0.35 + 0.65 * frac;
        sprite.custom_size = Some(shard.birth_size * s);
    }
}
