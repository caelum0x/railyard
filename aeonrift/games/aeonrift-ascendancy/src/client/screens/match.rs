//! The in-match screen.
//!
//! Unlike the other screens this one is REAL: the live battle is drawn by
//! [`super::super::render`] + [`super::super::hud`] and driven by
//! [`super::super::input`] + [`super::super::tick`], all a thin VIEW over the
//! deterministic core (`crate::MatchState`). This module bundles those systems into a
//! single [`Plugin`] so a screen router can add/remove the match as a unit.

use bevy::prelude::*;

use super::super::{hud, input, render, tick};

/// Plugin that wires the live-match systems (render + hud + input + fixed-step tick).
pub struct MatchScreenPlugin;

impl Plugin for MatchScreenPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Startup, hud::setup)
            .add_systems(FixedUpdate, tick::advance_match)
            .add_systems(
                Update,
                (
                    input::handle_input,
                    render::draw_battle,
                    hud::update_hand,
                    hud::update_elixir,
                    hud::update_bases,
                    hud::update_message,
                    hud::update_winner,
                ),
            );
    }
}
