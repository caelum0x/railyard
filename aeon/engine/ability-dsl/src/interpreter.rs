//! The interpreter / compiler: turn validated DSL data into live sim behaviour.
//!
//! Two responsibilities:
//!   1. [`compile`] — lower a [`ChampionSpec`]'s base stats into a [`sim::Champion`]
//!      (auto-attack damage, armor, range, attack period, move speed, HP).
//!   2. [`execute_ability`] — run one ability's effects deterministically by calling
//!      the M2 sim's public combat primitives. NOTHING here reimplements combat: it
//!      spawns projectiles, applies mitigated damage, heals, stuns, slows, dashes.
//!
//! Per-ability cooldowns live in [`ChampionRuntime`] (the DSL allows many abilities
//! per champion, while the sim's `Champion` tracks only its single built-in ability
//! cooldown). Cooldowns are plain integer counters advanced each tick, so they are
//! fully deterministic.

use sim::{EntityId, Vec2, World, TICK_HZ};

use crate::spec::{AbilitySpec, ChampionSpec, Effect, Scaling, Targeting};

/// Lower DSL base stats into a concrete sim champion at `pos` on `team`.
///
/// `attack_speed` (attacks/second) is compiled to an integer tick period,
/// `period = round(TICK_HZ / attack_speed)` (min 1 tick), so combat stays on the
/// sim's integer-exact cooldown clock.
pub fn compile(spec: &ChampionSpec, id: EntityId, team: u8, pos: Vec2) -> sim::Champion {
    let b = &spec.base;
    let period = ((TICK_HZ as f32) / b.attack_speed).round().max(1.0) as u32;
    sim::Champion::new(id, team, pos)
        .with_max_health(b.hp)
        .with_attack_damage(b.ad)
        .with_armor(b.armor)
        .with_attack_range(b.range)
        .with_attack_period(period)
        .with_move_speed(b.move_speed)
}

/// Runtime state for one champion in a running match: its spec plus a per-ability
/// cooldown counter (in ticks). Deterministic — just integers advanced each tick.
#[derive(Clone, Debug)]
pub struct ChampionRuntime {
    pub id: EntityId,
    pub spec: ChampionSpec,
    /// Remaining cooldown ticks per ability, indexed like `spec.abilities`.
    cooldowns: Vec<u32>,
}

impl ChampionRuntime {
    pub fn new(id: EntityId, spec: ChampionSpec) -> Self {
        let cooldowns = vec![0; spec.abilities.len()];
        ChampionRuntime {
            id,
            spec,
            cooldowns,
        }
    }

    /// Advance all ability cooldowns by one tick (saturating at zero).
    pub fn tick_cooldowns(&mut self) {
        for cd in &mut self.cooldowns {
            *cd = cd.saturating_sub(1);
        }
    }

    /// Is the ability at `index` ready to cast (off cooldown)?
    pub fn is_ready(&self, index: usize) -> bool {
        self.cooldowns.get(index).copied() == Some(0)
    }

    /// Put the ability at `index` on its full cooldown.
    pub fn put_on_cooldown(&mut self, index: usize) {
        if let Some(cd) = self.cooldowns.get_mut(index) {
            *cd = self.spec.abilities[index].cooldown;
        }
    }
}

/// Resolve an ability's `Damage` effect to a concrete integer amount, applying
/// optional stat scaling. Truncation keeps it integer-exact and deterministic.
fn resolved_damage(amount: i32, scaling: Option<Scaling>, caster_ad: i32) -> i32 {
    let bonus = match scaling {
        Some(Scaling::AttackDamage { ratio }) => (caster_ad as f32 * ratio) as i32,
        None => 0,
    };
    amount + bonus
}

/// Execute one ability deterministically inside `world`.
///
/// `aim` is the aim point / primary target position; `target_id` is the primary
/// enemy for `Target` abilities. Effect semantics (fixed across the vocabulary):
///   * `Damage` / `Slow` / `Stun` hit the *victim(s)* — the caster for `SelfCast`,
///     the `target_id` for `Target`, whoever a projectile hits for `Skillshot`,
///     and every enemy within `range` of `aim` for `Area`.
///   * `Heal` always restores the *caster*.
///   * `Dash` always moves the *caster* toward `aim`.
pub fn execute_ability(
    world: &mut World,
    caster_id: EntityId,
    ability: &AbilitySpec,
    aim: Vec2,
    target_id: Option<EntityId>,
) {
    let Some(caster) = world.champion(caster_id) else {
        return;
    };
    if !caster.alive || caster.stun_ticks > 0 {
        return;
    }
    let caster_ad = caster.attack_damage;

    match ability.targeting {
        Targeting::SelfCast => {
            for effect in &ability.effects {
                apply_to_victim(world, caster_id, caster_id, aim, caster_ad, effect);
            }
        }
        Targeting::Target => {
            for effect in &ability.effects {
                // Damage/CC need a victim; heal/dash act on the caster regardless.
                let victim = target_id.unwrap_or(caster_id);
                apply_to_victim(world, caster_id, victim, aim, caster_ad, effect);
            }
        }
        Targeting::Skillshot => cast_skillshot(world, caster_id, ability, aim, caster_ad),
        Targeting::Area => cast_area(world, caster_id, ability, aim, caster_ad),
    }
}

/// Apply a single effect where damage/CC land on `victim_id` and heal/dash act on
/// `caster_id`. This is the sole bridge from the DSL vocabulary to sim primitives.
fn apply_to_victim(
    world: &mut World,
    caster_id: EntityId,
    victim_id: EntityId,
    aim: Vec2,
    caster_ad: i32,
    effect: &Effect,
) {
    match *effect {
        Effect::Damage { amount, scaling } => {
            sim::apply_ability_damage(
                world,
                victim_id,
                resolved_damage(amount, scaling, caster_ad),
            );
        }
        Effect::Heal { amount } => sim::apply_heal(world, caster_id, amount),
        Effect::Slow { duration } => sim::apply_slow(world, victim_id, duration),
        Effect::Stun { duration } => sim::apply_stun(world, victim_id, duration),
        Effect::Dash { distance } => sim::apply_dash(world, caster_id, aim, distance),
    }
}

/// Skillshot: aggregate the ability's damage + crowd control into a single flying
/// projectile (via the sim). Heal/Dash effects on a skillshot act on the caster
/// immediately (e.g. a dashing nuke).
fn cast_skillshot(
    world: &mut World,
    caster_id: EntityId,
    ability: &AbilitySpec,
    aim: Vec2,
    caster_ad: i32,
) {
    let mut damage = 0i32;
    let mut stun = 0u32;
    let mut slow = 0u32;
    for effect in &ability.effects {
        match *effect {
            Effect::Damage { amount, scaling } => {
                damage += resolved_damage(amount, scaling, caster_ad)
            }
            Effect::Stun { duration } => stun = stun.max(duration),
            Effect::Slow { duration } => slow = slow.max(duration),
            Effect::Heal { amount } => sim::apply_heal(world, caster_id, amount),
            Effect::Dash { distance } => sim::apply_dash(world, caster_id, aim, distance),
        }
    }
    sim::spawn_projectile(world, caster_id, aim, damage, ability.range, stun, slow);
}

/// Area: instantly affect every living enemy within `range` of `aim`. Enemies are
/// resolved from a stable snapshot in id order for determinism.
fn cast_area(
    world: &mut World,
    caster_id: EntityId,
    ability: &AbilitySpec,
    aim: Vec2,
    caster_ad: i32,
) {
    let Some(caster) = world.champion(caster_id) else {
        return;
    };
    let team = caster.team;
    let radius = ability.range;
    let victims: Vec<EntityId> = world
        .champions
        .iter()
        .filter(|c| c.alive && c.team != team && c.pos.distance(aim) <= radius)
        .map(|c| c.id)
        .collect();

    for effect in &ability.effects {
        match *effect {
            Effect::Heal { amount } => sim::apply_heal(world, caster_id, amount),
            Effect::Dash { distance } => sim::apply_dash(world, caster_id, aim, distance),
            Effect::Damage { amount, scaling } => {
                let dmg = resolved_damage(amount, scaling, caster_ad);
                for &v in &victims {
                    sim::apply_ability_damage(world, v, dmg);
                }
            }
            Effect::Slow { duration } => {
                for &v in &victims {
                    sim::apply_slow(world, v, duration);
                }
            }
            Effect::Stun { duration } => {
                for &v in &victims {
                    sim::apply_stun(world, v, duration);
                }
            }
        }
    }
}
