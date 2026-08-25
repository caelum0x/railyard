//! The match runner: one deterministic tick, plus scripted and bot-driven matches.
//!
//! One tick is a fixed pipeline (the determinism law, same shape as `sim::step`):
//!   1. regenerate elixir (fixed period),
//!   2. apply this tick's deploys in the given order,
//!   3. compute unit movement intents (ascending id order),
//!   4. advance the shared sim world one tick (movement, combat, projectiles, death),
//!   5. evaluate the base-destroyed win condition.
//!
//! Same seed + same deploy decisions => byte-identical state and hash, every run.

use crate::ai::deploy_bot::bot_deploys;
use crate::rules::config::MatchConfig;
use crate::rules::deploy::{try_deploy, Deploy};
use crate::rules::elixir::regen_elixir;
use crate::rules::match_state::{MatchState, Outcome};
use crate::rules::unit_ai::movement_inputs;
use crate::rules::win;

use crate::cards::card::Card;

/// Default ticks between a bot's deploy attempts (~1s at 30 Hz).
pub const DEFAULT_DEPLOY_INTERVAL: u64 = 30;

/// Advance the match by exactly one tick, applying `deploys` (in order) first.
///
/// Deploy rejections (e.g. not enough elixir) are silently skipped here: a bot or
/// script may optimistically request more than it can afford, and the economy rule is
/// enforced by [`try_deploy`]. Call `try_deploy` directly when you need the reason.
pub fn step_tick(m: &mut MatchState, deploys: &[Deploy]) {
    if m.is_over() {
        return;
    }
    regen_elixir(m);
    for &d in deploys {
        let _ = try_deploy(m, d);
    }
    let inputs = movement_inputs(m);
    sim::step(&mut m.world, &inputs);
    m.outcome = win::evaluate(m);
}

/// Run a fully **scripted** match: `schedule` lists `(tick, Deploy)` pairs, applied on
/// their tick. Runs to a decisive outcome or the config's tick cap (then a draw).
pub fn run_scripted_match(
    config: MatchConfig,
    deck0: Vec<Card>,
    deck1: Vec<Card>,
    schedule: &[(u64, Deploy)],
) -> MatchState {
    let mut m = MatchState::new(config, deck0, deck1);
    while !m.is_over() && m.world.tick < config.max_ticks {
        let t = m.world.tick;
        let deploys: Vec<Deploy> = schedule
            .iter()
            .filter(|(dt, _)| *dt == t)
            .map(|(_, d)| *d)
            .collect();
        step_tick(&mut m, &deploys);
    }
    if !m.is_over() {
        m.outcome = Some(Outcome::Draw);
    }
    m
}

/// Run a deterministic **bot-vs-bot** match: on each deploy tick, each player deploys
/// its most expensive affordable card into a rotating lane (see
/// [`crate::ai::deploy_bot`]). Pure function of the config + decks => reproducible.
pub fn run_bot_match(
    config: MatchConfig,
    deck0: Vec<Card>,
    deck1: Vec<Card>,
    deploy_interval: u64,
) -> MatchState {
    let mut m = MatchState::new(config, deck0, deck1);
    while !m.is_over() && m.world.tick < config.max_ticks {
        let deploys = bot_deploys(&m, deploy_interval);
        step_tick(&mut m, &deploys);
    }
    if !m.is_over() {
        m.outcome = Some(Outcome::Draw);
    }
    m
}
