//! Advancing the match on a FIXED timestep by stepping the deterministic core.
//!
//! The client never forks the *rules*: each fixed tick it asks the core's own bot
//! policy (`crate::bot_deploys`) for player 1's move, then advances the shared sim
//! world through the core's public seams (`regen_elixir`, `try_deploy`,
//! `movement_inputs`, `sim::step`, `win::evaluate`).
//!
//! ## The VIEW enriches the sim with ability casts
//! The tested headless runner (`crate::step_tick`) issues only *movement* inputs, so a
//! bot-vs-bot match there is pure auto-attacks — that pipeline stays byte-identical and
//! keeps every determinism test green. The interactive client, however, also lets a
//! deployed unit CAST its skillshot ability when it is off cooldown with an enemy in
//! range (see [`cast_inputs`]). These are still real, deterministic sim inputs
//! (`sim::Command::CastSkillshot`) that spawn real projectiles — the client just opts
//! the extra verb in so the battlefield has abilities to *show* (and to hang VFX on).
//! It is a strict superset of the core tick, assembled here from the same public parts.

use bevy::prelude::*;

use sim::{Input, World, ABILITY_RANGE};

use super::game::{slug_for_card, Battle, BOT};

/// The fixed simulation rate — matches the deterministic core (`sim::TICK_HZ`).
pub const TICK_HZ: f64 = sim::TICK_HZ as f64;

/// The nearest living enemy of `unit_id` that lies within skillshot [`ABILITY_RANGE`],
/// as a world-space aim point. Deterministic: ties broken by ascending entity id.
fn ability_target(world: &World, unit_id: sim::EntityId) -> Option<sim::Vec2> {
    let me = world.champion(unit_id)?;
    let mut best: Option<(f32, sim::EntityId, sim::Vec2)> = None;
    for other in &world.champions {
        if other.id == me.id || !other.alive || other.team == me.team {
            continue;
        }
        let d = me.pos.distance(other.pos);
        if d > ABILITY_RANGE {
            continue;
        }
        let better = match best {
            None => true,
            Some((bd, bid, _)) => d < bd || (d == bd && other.id < bid),
        };
        if better {
            best = Some((d, other.id, other.pos));
        }
    }
    best.map(|(_, _, pos)| pos)
}

/// Cast inputs for every living, deployed unit whose ability is ready and which has an
/// enemy in range — in ascending entity-id order, so the whole client tick stays
/// deterministic. Bases never cast (they are the win-condition target, not attackers).
pub fn cast_inputs(battle: &Battle) -> Vec<Input> {
    let m = &battle.state;
    let mut inputs = Vec::new();
    for champ in &m.world.champions {
        if m.is_base(champ.id) || !champ.alive || champ.ability_cooldown != 0 {
            continue;
        }
        if let Some(aim) = ability_target(&m.world, champ.id) {
            inputs.push(Input::cast_skillshot(champ.id, aim));
        }
    }
    inputs
}

/// Advance the match by exactly one client tick: the core pipeline
/// (regen -> deploy -> movement) PLUS ability casts, then one `sim::step` and the win
/// check. Bot deploys are captured here (not delegated to `step_tick`) so the new
/// unit's slug can be tagged for face rendering.
pub fn step_once(battle: &mut Battle) {
    if battle.state.is_over() {
        return;
    }
    let interval = battle.bot_interval;

    // 1) Elixir regen (core rule).
    crate::regen_elixir(&mut battle.state);

    // 2) Bot deploys (core policy) — applied here so we can tag slugs.
    let deploys: Vec<_> = crate::bot_deploys(&battle.state, interval)
        .into_iter()
        .filter(|d| d.player == BOT)
        .collect();
    for d in deploys {
        if let Ok(id) = crate::try_deploy(&mut battle.state, d) {
            if let Some(slug) = slug_for_card(d.card) {
                battle.record_unit(id, slug);
            }
        }
    }

    // 3) Movement + ability casts (the VIEW's superset), applied in one sim step.
    let mut inputs = crate::movement_inputs(&battle.state);
    inputs.extend(cast_inputs(battle));
    sim::step(&mut battle.state.world, &inputs);

    // 4) Win condition (core rule).
    battle.state.outcome = crate::rules::win::evaluate(&battle.state);
}

/// `FixedUpdate` system wrapper for the windowed app.
pub fn advance_match(mut battle: ResMut<Battle>) {
    step_once(&mut battle);
}
