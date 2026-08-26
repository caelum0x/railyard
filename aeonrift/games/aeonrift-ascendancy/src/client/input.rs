//! Player input: tap/click a card to select it, then tap a lane to deploy.
//!
//! Mouse (desktop) and touch (mobile) are handled uniformly — both produce a "press"
//! at a screen position, which is converted to a world position via the camera. A
//! press either selects a hand card or, with a card selected, deploys it into the
//! nearest lane by calling the deterministic core's `crate::try_deploy`. The core's
//! `DeployError` (e.g. not enough elixir) is surfaced as a transient status message.
//! This system is the ONLY place the client mutates match state from player intent.

use bevy::prelude::*;

use crate::{try_deploy, Deploy, DeployError};

use super::game::{
    Battle, Selection, StatusMessage, CARD_GAP, CARD_H, CARD_ROW_Y, CARD_W, FIELD_BOTTOM, HUMAN,
};

/// Collect this frame's press positions (screen space) from mouse + touch.
fn press_positions(
    buttons: &ButtonInput<MouseButton>,
    touches: &Touches,
    window: &Window,
) -> Vec<Vec2> {
    let mut points = Vec::new();
    if buttons.just_pressed(MouseButton::Left) {
        if let Some(p) = window.cursor_position() {
            points.push(p);
        }
    }
    for touch in touches.iter_just_pressed() {
        points.push(touch.position());
    }
    points
}

/// The x-center of hand card `i` for a deck of `n` cards (centered row).
pub fn card_center_x(i: usize, n: usize) -> f32 {
    let step = CARD_W + CARD_GAP;
    let total = step * n as f32 - CARD_GAP;
    let start = -total / 2.0 + CARD_W / 2.0;
    start + i as f32 * step
}

/// Hit-test a world point against the hand cards; returns the card index if any.
fn card_at(point: Vec2, n: usize) -> Option<usize> {
    if (point.y - CARD_ROW_Y).abs() > CARD_H / 2.0 {
        return None;
    }
    (0..n).find(|&i| (point.x - card_center_x(i, n)).abs() <= CARD_W / 2.0)
}

/// The lane index whose x-coordinate is nearest to `x` (Clash-style: tap the field,
/// the unit joins the closest lane).
fn nearest_lane(battle: &Battle, x: f32) -> usize {
    let lanes = battle.state.config.lanes.max(1);
    (0..lanes)
        .min_by(|&a, &b| {
            let da = (battle.state.config.lane_x(a) - x).abs();
            let db = (battle.state.config.lane_x(b) - x).abs();
            da.total_cmp(&db)
        })
        .unwrap_or(0)
}

/// Turn a `DeployError` from the core into a short, user-facing message. Shared with
/// the keyboard/action deploy path in [`super::actions`].
pub(crate) fn error_message(err: DeployError) -> String {
    match err {
        DeployError::NotEnoughElixir { have, need } => {
            format!("Not enough elixir ({have}/{need})")
        }
        DeployError::MatchOver => "Match is over".to_string(),
        DeployError::BadLane => "Invalid lane".to_string(),
        DeployError::BadCard => "Invalid card".to_string(),
        DeployError::BadPlayer => "Invalid player".to_string(),
    }
}

/// The input system: reads presses, updates selection, and drives `try_deploy`.
pub fn handle_input(
    buttons: Res<ButtonInput<MouseButton>>,
    touches: Res<Touches>,
    windows: Query<&Window>,
    camera: Query<(&Camera, &GlobalTransform)>,
    mut battle: ResMut<Battle>,
    mut selection: ResMut<Selection>,
    mut status: ResMut<StatusMessage>,
) {
    let Ok(window) = windows.single() else {
        return;
    };
    let Ok((camera, cam_tf)) = camera.single() else {
        return;
    };

    let deck_len = battle.state.decks[HUMAN as usize].len();

    for screen_pos in press_positions(&buttons, &touches, window) {
        // Uniform mouse/touch -> world conversion through the camera.
        let Ok(world) = camera.viewport_to_world_2d(cam_tf, screen_pos) else {
            continue;
        };

        // 1) Tapped a hand card -> (re)select, or toggle off if already selected.
        if let Some(idx) = card_at(world, deck_len) {
            selection.card = if selection.card == Some(idx) {
                None
            } else {
                Some(idx)
            };
            continue;
        }

        // 2) Tapped the field with a card selected -> deploy into the nearest lane.
        if world.y > FIELD_BOTTOM {
            if let Some(card) = selection.card {
                let lane = nearest_lane(&battle, world.x);
                let deploy = Deploy::new(HUMAN, card, lane);
                match try_deploy(&mut battle.state, deploy) {
                    Ok(id) => {
                        let name = battle.state.decks[HUMAN as usize][card].name().to_string();
                        // Tag the new unit with its slug so the board can draw its face.
                        if let Some(slug) = super::game::slug_for_card(card) {
                            battle.record_unit(id, slug);
                        }
                        status.set(format!("Deployed {name} -> lane {}", lane + 1));
                        selection.card = None;
                    }
                    Err(e) => status.set(error_message(e)),
                }
            } else {
                status.set("Select a card first");
            }
        }
    }
}
