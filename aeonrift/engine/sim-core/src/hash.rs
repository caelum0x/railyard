//! Deterministic state hashing (FNV-1a, 64-bit).
//!
//! Hand-rolled so the hash is stable across runs and independent of std's hasher
//! internals. Floats are hashed by their raw bits (`to_bits`) so equal states
//! produce equal hashes and any drift is detected.

use crate::world::World;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

struct Fnv1a {
    hash: u64,
}

impl Fnv1a {
    fn new() -> Self {
        Fnv1a { hash: FNV_OFFSET }
    }

    fn write_u8(&mut self, b: u8) {
        self.hash ^= b as u64;
        self.hash = self.hash.wrapping_mul(FNV_PRIME);
    }

    fn write_bytes(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.write_u8(b);
        }
    }

    fn write_u32(&mut self, v: u32) {
        self.write_bytes(&v.to_le_bytes());
    }

    fn write_i32(&mut self, v: i32) {
        self.write_bytes(&v.to_le_bytes());
    }

    fn write_u64(&mut self, v: u64) {
        self.write_bytes(&v.to_le_bytes());
    }

    fn write_f32(&mut self, v: f32) {
        // Hash raw bits so NaN/-0.0 are handled deterministically and equal
        // floats hash equally.
        self.write_u32(v.to_bits());
    }

    fn finish(self) -> u64 {
        self.hash
    }
}

/// Compute a 64-bit hash of the full world state. Two worlds with byte-identical
/// content produce the same hash; any difference produces a different hash with
/// overwhelming probability.
pub fn state_hash(world: &World) -> u64 {
    let mut h = Fnv1a::new();
    h.write_u64(world.tick);
    h.write_u64(world.rng.state());
    h.write_u32(world.next_projectile_id);

    h.write_u32(world.champions.len() as u32);
    for c in &world.champions {
        h.write_u32(c.id);
        h.write_u8(c.team);
        h.write_f32(c.pos.x);
        h.write_f32(c.pos.y);
        match c.target {
            Some(t) => {
                h.write_u8(1);
                h.write_f32(t.x);
                h.write_f32(t.y);
            }
            None => h.write_u8(0),
        }
        // Combat state.
        h.write_i32(c.health.current);
        h.write_i32(c.health.max);
        h.write_i32(c.attack_damage);
        h.write_i32(c.armor);
        h.write_f32(c.attack_range);
        h.write_u32(c.attack_period);
        h.write_u32(c.attack_cooldown);
        h.write_u32(c.ability_cooldown);
        h.write_f32(c.move_speed);
        h.write_u32(c.stun_ticks);
        h.write_u32(c.slow_ticks);
        h.write_u8(c.alive as u8);
    }

    h.write_u32(world.projectiles.len() as u32);
    for p in &world.projectiles {
        h.write_u32(p.id);
        h.write_u32(p.owner);
        h.write_u8(p.team);
        h.write_f32(p.pos.x);
        h.write_f32(p.pos.y);
        h.write_f32(p.vel.x);
        h.write_f32(p.vel.y);
        h.write_i32(p.damage);
        h.write_f32(p.remaining_range);
        h.write_f32(p.hit_radius);
        h.write_u32(p.stun_on_hit);
        h.write_u32(p.slow_on_hit);
    }

    h.finish()
}
