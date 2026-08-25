//! Deterministic systems: input application, movement, and the M2 combat core.
//!
//! Every system iterates entities in stable id order and uses only fixed-timestep
//! math and integer combat arithmetic, so two runs from the same seed + inputs
//! produce byte-identical state (the determinism gate). Damage is decided from a
//! read-only snapshot and then applied in a deterministic order, so mid-tick
//! deaths never depend on iteration/borrow accidents.

use crate::input::{Command, EntityId, Input};
use crate::math::Vec2;
use crate::world::{
    Champion, Projectile, World, ARRIVE_TOLERANCE, DT, MAP_BOUNDS, PROJECTILE_SPEED,
    SLOW_MOVE_FACTOR,
};

/// Armor mitigation formula: `effective = raw * 100 / (100 + armor)`.
///
/// Pure integer math (i64 intermediate to avoid overflow), so it is exactly
/// reproducible on every machine. Armor is floored so it never divides by <= 0.
pub fn mitigated_damage(raw: i32, armor: i32) -> i32 {
    let denom = (100 + armor).max(1) as i64;
    ((raw as i64 * 100) / denom) as i32
}

/// Authoritative input validation. Clamps aim/move points into the play-plane.
///
/// The server applies this to every input it accepts. The client deliberately
/// predicts with the *raw* (unvalidated) input so the reconciliation path is
/// exercised by real authoritative corrections (see README.md "Netcode spine").
pub fn validate_input(input: Input) -> Input {
    match input.command {
        Command::MoveTo(target) => Input {
            entity: input.entity,
            command: Command::MoveTo(target.clamp_square(MAP_BOUNDS)),
        },
        Command::CastSkillshot(aim) => Input {
            entity: input.entity,
            command: Command::CastSkillshot(aim.clamp_square(MAP_BOUNDS)),
        },
    }
}

/// Apply intents to entity state. Inputs are applied in the given order, so the
/// caller must provide them in a deterministic order (e.g. sorted by seq). Dead
/// champions ignore all input.
pub fn apply_inputs(world: &mut World, inputs: &[Input]) {
    for input in inputs {
        let Some(idx) = world.champions.iter().position(|c| c.id == input.entity) else {
            continue;
        };
        // Dead or stunned champions ignore all input.
        if !world.champions[idx].alive || world.champions[idx].stun_ticks > 0 {
            continue;
        }
        match input.command {
            Command::MoveTo(target) => world.champions[idx].target = Some(target),
            Command::CastSkillshot(aim) => cast_skillshot(world, idx, aim),
        }
    }
}

/// Spawn a skillshot projectile from champion at `idx` toward `aim`, gated by the
/// caster's ability cooldown. A zero-length aim (aiming at your own feet) is a
/// no-op so no degenerate zero-velocity projectile is created.
fn cast_skillshot(world: &mut World, idx: usize, aim: Vec2) {
    let caster = world.champions[idx];
    if caster.ability_cooldown != 0 {
        return; // on cooldown: cast is rejected deterministically
    }
    let dir = (aim - caster.pos).normalized();
    if dir == Vec2::ZERO {
        return;
    }
    let id = world.next_projectile_id;
    world.next_projectile_id += 1;
    world.projectiles.push(Projectile {
        id,
        owner: caster.id,
        team: caster.team,
        pos: caster.pos,
        vel: dir * PROJECTILE_SPEED,
        damage: crate::world::ABILITY_DAMAGE,
        remaining_range: crate::world::ABILITY_RANGE,
        hit_radius: crate::world::PROJECTILE_HIT_RADIUS,
        stun_on_hit: 0,
        slow_on_hit: 0,
    });
    world.champions[idx].ability_cooldown = crate::world::ABILITY_COOLDOWN_TICKS;
}

/// Move each living champion toward its target by one fixed step, with "arrive":
/// when within one step (or tolerance) of the target, snap exactly onto it and
/// stop. Snapping guarantees the final resting position is bit-identical
/// regardless of the path taken — critical for netcode convergence.
pub fn movement_system(world: &mut World) {
    for champ in world.champions.iter_mut() {
        // Dead or stunned champions do not move (a stunned champion keeps its
        // pending move target and resumes once the stun expires).
        if !champ.alive || champ.stun_ticks > 0 {
            continue;
        }
        // Per-champion speed, reduced while slowed. Computed each tick so status
        // changes take effect immediately and deterministically.
        let speed = if champ.slow_ticks > 0 {
            champ.move_speed * SLOW_MOVE_FACTOR
        } else {
            champ.move_speed
        };
        let step = speed * DT;
        if let Some(target) = champ.target {
            let to = target - champ.pos;
            let dist = to.length();
            if dist <= step || dist <= ARRIVE_TOLERANCE {
                champ.pos = target; // arrive: exact snap, then stop
                champ.target = None;
            } else {
                let dir: Vec2 = to * (1.0 / dist);
                champ.pos = champ.pos + dir * step;
            }
        }
    }
}

/// Tick down every cooldown by one (saturating at zero) for living champions.
/// Runs before the attack/ability systems so a champion whose cooldown reaches
/// zero this tick may act this tick.
pub fn cooldown_system(world: &mut World) {
    for champ in world.champions.iter_mut() {
        if !champ.alive {
            continue;
        }
        champ.attack_cooldown = champ.attack_cooldown.saturating_sub(1);
        champ.ability_cooldown = champ.ability_cooldown.saturating_sub(1);
        // Status effects also tick down here, so a stun/slow lasts exactly the
        // number of ticks it was applied for.
        champ.stun_ticks = champ.stun_ticks.saturating_sub(1);
        champ.slow_ticks = champ.slow_ticks.saturating_sub(1);
    }
}

/// Advance projectiles one fixed step, resolve collisions, and apply damage.
///
/// Determinism: projectiles are processed in id order; each resolves against the
/// lowest-id enemy champion within its hit radius. Hit/expired projectiles are
/// removed, then damage is applied in projectile-id order.
pub fn projectile_system(world: &mut World) {
    // Read-only snapshot of champion collision data, in id order.
    let targets: Vec<(EntityId, u8, Vec2, bool)> = world
        .champions
        .iter()
        .map(|c| (c.id, c.team, c.pos, c.alive))
        .collect();

    // Each event: (target id, raw damage, stun ticks, slow ticks).
    let mut hit_events: Vec<(EntityId, i32, u32, u32)> = Vec::new();
    let mut remove: Vec<EntityId> = Vec::new();

    for proj in world.projectiles.iter_mut() {
        let disp = proj.vel * DT;
        proj.pos = proj.pos + disp;
        proj.remaining_range -= disp.length();

        // Collision: first (lowest-id) living enemy within the hit radius.
        let mut hit: Option<EntityId> = None;
        for (id, team, pos, alive) in &targets {
            if !*alive || *team == proj.team {
                continue;
            }
            if proj.pos.distance(*pos) <= proj.hit_radius {
                hit = Some(*id);
                break;
            }
        }

        if let Some(tid) = hit {
            hit_events.push((tid, proj.damage, proj.stun_on_hit, proj.slow_on_hit));
            remove.push(proj.id);
        } else if proj.remaining_range <= 0.0 {
            remove.push(proj.id); // travelled its full range without a hit: a miss
        }
    }

    // Retain projectiles that neither hit nor expired (order preserved).
    if !remove.is_empty() {
        world.projectiles.retain(|p| !remove.contains(&p.id));
    }

    // Apply damage + crowd control in projectile-id order (deterministic).
    for (tid, raw, stun, slow) in hit_events {
        apply_damage(world, tid, raw);
        // Only living targets receive crowd control (a target killed by this hit
        // stays dead/inert rather than carrying a status).
        if stun > 0 {
            apply_stun(world, tid, stun);
        }
        if slow > 0 {
            apply_slow(world, tid, slow);
        }
    }
}

/// Auto-attack system: every living champion whose attack is off cooldown and has
/// a living enemy in range deals mitigated damage to the nearest such enemy
/// (ties broken by lowest id) and goes on cooldown.
///
/// Attacks are decided from a read-only snapshot, then applied in attacker-id
/// order, so simultaneous exchanges are symmetric and deterministic.
pub fn auto_attack_system(world: &mut World) {
    let snap: Vec<Champion> = world.champions.clone();

    let mut fired: Vec<EntityId> = Vec::new();
    let mut damage_events: Vec<(EntityId, i32)> = Vec::new();

    for attacker in &snap {
        if !attacker.alive || attacker.stun_ticks > 0 || attacker.attack_cooldown > 0 {
            continue;
        }
        let mut best: Option<(EntityId, f32)> = None;
        for defender in &snap {
            if defender.id == attacker.id || !defender.alive || defender.team == attacker.team {
                continue;
            }
            let d = attacker.pos.distance(defender.pos);
            if d <= attacker.attack_range && best.map(|(_, bd)| d < bd).unwrap_or(true) {
                best = Some((defender.id, d));
            }
        }
        if let Some((tid, _)) = best {
            fired.push(attacker.id);
            damage_events.push((tid, attacker.attack_damage));
        }
    }

    // Put attackers that fired on cooldown.
    for aid in fired {
        if let Some(a) = world.champion_mut(aid) {
            a.attack_cooldown = a.attack_period;
        }
    }
    // Apply damage in attacker-id order.
    for (tid, raw) in damage_events {
        apply_damage(world, tid, raw);
    }
}

/// Apply mitigated `raw` damage to `target_id`. Already-dead targets are skipped
/// (so a target killed earlier this tick takes no further damage). A champion
/// that drops to <= 0 HP is marked dead and stops acting (its move target is
/// cleared); dead champions are retained in the roster so ids/hashing stay stable.
fn apply_damage(world: &mut World, target_id: EntityId, raw: i32) {
    if let Some(t) = world.champion_mut(target_id) {
        if !t.alive {
            return;
        }
        let dmg = mitigated_damage(raw, t.armor);
        t.health = t.health.damaged(dmg);
        if t.health.is_dead() {
            t.alive = false;
            t.target = None;
        }
    }
}

// ---------------------------------------------------------------------------
// Public effect primitives (M5 DSL "effect vocabulary" compiles DOWN to these).
//
// These are the closed set of combat verbs the champion DSL can emit. Each one
// reuses the M2 combat core (armor mitigation, integer HP, death handling) —
// the DSL does NOT reimplement combat, it just orchestrates these primitives.
// ---------------------------------------------------------------------------

/// Apply raw (pre-mitigation) ability damage to a target, going through the same
/// armor formula and death handling as an auto-attack. No-op on a dead target.
pub fn apply_ability_damage(world: &mut World, target_id: EntityId, raw: i32) {
    apply_damage(world, target_id, raw);
}

/// Heal a living target by `amount`, clamped so it never exceeds max HP. No-op on
/// a dead target (you cannot heal the dead — that keeps HP monotonic on death).
pub fn apply_heal(world: &mut World, target_id: EntityId, amount: i32) {
    if amount <= 0 {
        return;
    }
    if let Some(t) = world.champion_mut(target_id) {
        if !t.alive {
            return;
        }
        let healed = (t.health.current + amount).min(t.health.max);
        t.health = crate::world::Health {
            current: healed,
            max: t.health.max,
        };
    }
}

/// Stun a living target for `ticks` (takes the longer of the current and new
/// remaining stun, so overlapping stuns do not shorten an existing one).
pub fn apply_stun(world: &mut World, target_id: EntityId, ticks: u32) {
    if ticks == 0 {
        return;
    }
    if let Some(t) = world.champion_mut(target_id) {
        if !t.alive {
            return;
        }
        t.stun_ticks = t.stun_ticks.max(ticks);
    }
}

/// Slow a living target for `ticks` (takes the longer of current/new remaining).
pub fn apply_slow(world: &mut World, target_id: EntityId, ticks: u32) {
    if ticks == 0 {
        return;
    }
    if let Some(t) = world.champion_mut(target_id) {
        if !t.alive {
            return;
        }
        t.slow_ticks = t.slow_ticks.max(ticks);
    }
}

/// Dash a living champion up to `distance` units toward `toward`, clamped to the
/// play-plane and never overshooting the destination. Returns the new position.
/// Reuses the deterministic `Vec2` math; a zero-length direction is a no-op.
pub fn apply_dash(world: &mut World, caster_id: EntityId, toward: Vec2, distance: f32) {
    if let Some(c) = world.champion_mut(caster_id) {
        if !c.alive || c.stun_ticks > 0 {
            return;
        }
        let to = toward - c.pos;
        let len = to.length();
        if len == 0.0 || distance <= 0.0 {
            return;
        }
        let travel = distance.min(len); // never overshoot the aim point
        let dir = to * (1.0 / len);
        c.pos = (c.pos + dir * travel).clamp_square(MAP_BOUNDS);
    }
}

/// Spawn a skillshot projectile from `owner_id` toward `aim`, carrying explicit
/// damage and optional crowd control. Unlike the built-in [`cast_skillshot`], this
/// does NOT consult the M2 single-ability cooldown — DSL abilities track their own
/// per-ability cooldowns in the interpreter. Returns `true` if a projectile was
/// spawned (a zero-length aim is a deterministic no-op that returns `false`).
#[allow(clippy::too_many_arguments)]
pub fn spawn_projectile(
    world: &mut World,
    owner_id: EntityId,
    aim: Vec2,
    damage: i32,
    range: f32,
    stun_on_hit: u32,
    slow_on_hit: u32,
) -> bool {
    let Some(owner) = world.champion(owner_id) else {
        return false;
    };
    if !owner.alive || owner.stun_ticks > 0 {
        return false;
    }
    let dir = (aim - owner.pos).normalized();
    if dir == Vec2::ZERO {
        return false;
    }
    let (pos, team) = (owner.pos, owner.team);
    let id = world.next_projectile_id;
    world.next_projectile_id += 1;
    world.projectiles.push(Projectile {
        id,
        owner: owner_id,
        team,
        pos,
        vel: dir * PROJECTILE_SPEED,
        damage,
        remaining_range: range,
        hit_radius: crate::world::PROJECTILE_HIT_RADIUS,
        stun_on_hit,
        slow_on_hit,
    });
    true
}
