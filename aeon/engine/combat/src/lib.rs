//! # aeonrift-combat
//!
//! ROLE: Stats, damage/armor, buffs, crowd control, projectiles, on-hit resolution.
//!
//! MIGRATED: the deterministic combat primitives now live in `aeonrift-sim-core`
//! (migrated from moba/sim, which keeps combat and world in one deterministic
//! kernel). This crate is a thin facade that re-exports those primitives so
//! downstream code can depend on `aeonrift-combat` for the combat surface alone.
//!
//! DEPENDS_ON (workspace path deps):
//!   - `aeonrift-sim-core`
//!
//! GRADUATE-LATER: a richer standalone combat layer (moon-lol lol_core damage.rs,
//! buffs/, action/) remains a documented graduate-later track; see MIGRATION.md.

// Re-export the deterministic combat primitives from the sim kernel.
pub use sim::{
    apply_ability_damage, apply_dash, apply_heal, apply_slow, apply_stun, mitigated_damage,
    spawn_projectile, Champion, Health, Projectile, ABILITY_COOLDOWN_TICKS, ABILITY_DAMAGE,
    ABILITY_RANGE, PROJECTILE_HIT_RADIUS, PROJECTILE_SPEED, SLOW_MOVE_FACTOR,
};

#[cfg(test)]
mod tests {
    #[test]
    fn reexports_combat_primitives() {
        // Proves the facade links against the sim-core combat surface.
        let dmg = super::mitigated_damage(100, 50);
        assert!(dmg > 0 && dmg <= 100);
    }
}
