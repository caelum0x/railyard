//! Advancing the match on a FIXED timestep by stepping the deterministic core.
//!
//! The client never forks the simulation: each fixed tick it asks the core's own bot
//! policy (`crate::bot_deploys`) for the bot's move, keeps only the bot's deploy, and
//! calls `crate::step_tick`. The human's deploys are committed immediately in the
//! `input` module via `crate::try_deploy`; `step_tick` then advances the shared sim
//! world (movement + combat + projectiles + death) and evaluates the win condition.

use bevy::prelude::*;

use crate::step_tick;

use super::game::{Battle, BOT};

/// The fixed simulation rate — matches the deterministic core (`sim::TICK_HZ`).
pub const TICK_HZ: f64 = sim::TICK_HZ as f64;

/// Advance the match by exactly one core tick. Pure over the core: reuses the bot
/// policy for player 1 only (the human acts through `try_deploy`), then steps.
pub fn step_once(battle: &mut Battle) {
    if battle.state.is_over() {
        return;
    }
    let interval = battle.bot_interval;
    let deploys: Vec<_> = crate::bot_deploys(&battle.state, interval)
        .into_iter()
        .filter(|d| d.player == BOT)
        .collect();
    step_tick(&mut battle.state, &deploys);
}

/// `FixedUpdate` system wrapper for the windowed app.
pub fn advance_match(mut battle: ResMut<Battle>) {
    step_once(&mut battle);
}
