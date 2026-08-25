//! Spec-driven duel-test harness — the auto-verification gate for new champions.
//!
//! Given two champion specs and a seed, [`run_duel`] plays a fully deterministic
//! 1v1 bot match inside the sim and returns an [`DuelResult`] you can assert on
//! (winner, remaining HP, tick count, final state hash). This is what lets an agent
//! author a champion as data and get a green/red signal without human review.
//!
//! ## Determinism
//! The bot AI is pure and ordered (champions act in ascending id order; nearest-
//! enemy ties break by lowest id; every decision is integer/threshold based). The
//! sim itself is deterministic. So `run_duel(a, b, seed)` twice yields an identical
//! result — the property the duel-determinism test locks in.

use sim::{state_hash, step, EntityId, Input, Vec2, World};

use crate::interpreter::{compile, execute_ability, ChampionRuntime};
use crate::spec::{ChampionSpec, Targeting};

/// Cast a `SelfCast` ability (e.g. a heal) once the champion drops below this HP
/// fraction. Keeps self-sustain from being wasted at full health.
const SELF_CAST_HP_FRAC: f32 = 0.6;

/// Tunables for a duel. Defaults give a decisive lane fight.
#[derive(Clone, Copy, Debug)]
pub struct DuelConfig {
    /// RNG seed for the world (combat is RNG-free, but the seed is part of the
    /// world state + hash, so it is threaded through for a faithful contract).
    pub seed: u64,
    /// Hard cap on ticks; if neither champion dies by then the duel is a draw.
    pub max_ticks: u64,
    /// Initial separation: champion A at `(-d/2, 0)`, champion B at `(+d/2, 0)`.
    pub start_distance: f32,
}

impl Default for DuelConfig {
    fn default() -> Self {
        DuelConfig {
            seed: 42,
            max_ticks: 3000,
            start_distance: 30.0,
        }
    }
}

/// The assertable outcome of a duel.
#[derive(Clone, Debug, PartialEq)]
pub struct DuelResult {
    /// Winner's entity id, or `None` for a draw / timeout.
    pub winner: Option<EntityId>,
    /// Winner's champion name, or `None` for a draw / timeout.
    pub winner_name: Option<String>,
    /// Final HP per champion as `(id, current_hp)`, in id order.
    pub hp: Vec<(EntityId, i32)>,
    /// Ticks elapsed until the duel resolved (or `max_ticks` on a draw).
    pub ticks: u64,
    /// Full sim state hash at the decisive tick — an extra determinism fingerprint.
    pub final_hash: u64,
}

impl DuelResult {
    /// Convenience: current HP of a specific champion id.
    pub fn hp_of(&self, id: EntityId) -> i32 {
        self.hp
            .iter()
            .find(|(cid, _)| *cid == id)
            .map(|(_, hp)| *hp)
            .unwrap_or(0)
    }
}

/// Play a deterministic 1v1 duel between two authored specs.
pub fn run_duel(spec_a: &ChampionSpec, spec_b: &ChampionSpec, cfg: DuelConfig) -> DuelResult {
    let half = cfg.start_distance * 0.5;
    let champ_a = compile(spec_a, 0, 0, Vec2::new(-half, 0.0));
    let champ_b = compile(spec_b, 1, 1, Vec2::new(half, 0.0));
    let mut world = World::from_champions(cfg.seed, vec![champ_a, champ_b]);

    let mut runtimes = [
        ChampionRuntime::new(0, spec_a.clone()),
        ChampionRuntime::new(1, spec_b.clone()),
    ];

    let mut elapsed = cfg.max_ticks;
    for t in 0..cfg.max_ticks {
        for rt in &mut runtimes {
            rt.tick_cooldowns();
        }

        // Phase A: casts, in id order (a cast may kill or dash before the next).
        for rt in runtimes.iter_mut() {
            decide_and_cast(&mut world, rt);
        }

        // Phase B: movement intents, computed from the post-cast world in id order.
        let mut inputs: Vec<Input> = Vec::new();
        for rt in &runtimes {
            if let Some(input) = decide_move(&world, rt.id) {
                inputs.push(input);
            }
        }

        step(&mut world, &inputs);

        // Decisive as soon as at most one champion remains alive.
        let alive: Vec<EntityId> = world
            .champions
            .iter()
            .filter(|c| c.alive)
            .map(|c| c.id)
            .collect();
        if alive.len() <= 1 {
            elapsed = t + 1;
            break;
        }
    }

    let winner = world
        .champions
        .iter()
        .find(|c| c.alive)
        .filter(|_| world.champions.iter().filter(|c| c.alive).count() == 1)
        .map(|c| c.id);
    let winner_name = winner.map(|id| runtimes[id as usize].spec.name.clone());
    let hp = world
        .champions
        .iter()
        .map(|c| (c.id, c.health.current))
        .collect();

    DuelResult {
        winner,
        winner_name,
        hp,
        ticks: elapsed,
        final_hash: state_hash(&world),
    }
}

/// Pick and execute at most one ready ability for `rt`'s champion this tick.
fn decide_and_cast(world: &mut World, rt: &mut ChampionRuntime) {
    let Some(champ) = world.champion(rt.id) else {
        return;
    };
    if !champ.alive || champ.stun_ticks > 0 {
        return;
    }
    let (hp_frac, self_pos) = (
        champ.health.current as f32 / champ.health.max as f32,
        champ.pos,
    );

    let Some((enemy_id, enemy_pos, dist)) = nearest_enemy(world, rt.id) else {
        return;
    };

    // First castable ability wins (author orders abilities by priority).
    for i in 0..rt.spec.abilities.len() {
        if !rt.is_ready(i) {
            continue;
        }
        let ability = &rt.spec.abilities[i];
        let castable = match ability.targeting {
            Targeting::SelfCast => hp_frac < SELF_CAST_HP_FRAC,
            Targeting::Target | Targeting::Skillshot | Targeting::Area => dist <= ability.range,
        };
        if !castable {
            continue;
        }
        let aim = match ability.targeting {
            Targeting::SelfCast => self_pos,
            _ => enemy_pos,
        };
        let ability = ability.clone();
        execute_ability(world, rt.id, &ability, aim, Some(enemy_id));
        rt.put_on_cooldown(i);
        return;
    }
}

/// Movement intent: close on the nearest enemy until within auto-attack range,
/// then hold position (so the auto-attack system can fire).
fn decide_move(world: &World, id: EntityId) -> Option<Input> {
    let champ = world.champion(id)?;
    if !champ.alive || champ.stun_ticks > 0 {
        return None;
    }
    let (_, enemy_pos, dist) = nearest_enemy(world, id)?;
    if dist > champ.attack_range {
        Some(Input::move_to(id, enemy_pos))
    } else {
        None
    }
}

/// Nearest living enemy of `id`: returns `(enemy_id, enemy_pos, distance)`, ties
/// broken by lowest id (stable iteration order).
fn nearest_enemy(world: &World, id: EntityId) -> Option<(EntityId, Vec2, f32)> {
    let me = world.champion(id)?;
    let mut best: Option<(EntityId, Vec2, f32)> = None;
    for other in &world.champions {
        if other.id == id || !other.alive || other.team == me.team {
            continue;
        }
        let d = me.pos.distance(other.pos);
        if best.map(|(_, _, bd)| d < bd).unwrap_or(true) {
            best = Some((other.id, other.pos, d));
        }
    }
    best
}
