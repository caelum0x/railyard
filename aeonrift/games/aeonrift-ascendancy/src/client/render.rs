//! Immediate-mode battlefield rendering with gizmos.
//!
//! Every frame this reads the deterministic core's `sim::World` (via `Battle`) and
//! draws lanes, each deployed unit (a team-colored disc scaled by HP with a health
//! bar), and both bases' health bars. Nothing here mutates state — it is a pure view
//! of whatever the core computed on the last fixed tick.

use bevy::prelude::*;

use super::game::{
    blend, champion_tint, sim_to_view, team_color, Battle, Selection, CARD_H, CARD_ROW_Y, CARD_W,
    UNIT_RADIUS,
};
use super::input::card_center_x;

/// A short health bar centered above `pos`, `frac` filled (0..=1) in `color`.
fn health_bar(gizmos: &mut Gizmos, pos: Vec2, y_off: f32, width: f32, frac: f32, color: Color) {
    let y = pos.y + y_off;
    let left = pos.x - width / 2.0;
    let right = pos.x + width / 2.0;
    gizmos.line_2d(
        Vec2::new(left, y),
        Vec2::new(right, y),
        Color::srgb(0.1, 0.1, 0.1),
    );
    if frac > 0.0 {
        let end = left + width * frac.clamp(0.0, 1.0);
        gizmos.line_2d(Vec2::new(left, y), Vec2::new(end, y), color);
    }
}

/// Draw the lanes, all deployed units, base health bars, and the selection outline.
pub fn draw_battle(mut gizmos: Gizmos, battle: Res<Battle>, selection: Res<Selection>) {
    let cfg = &battle.state.config;

    // Lane guides: faint vertical lines spanning between the two bases.
    let y0 = -cfg.base_y;
    let y1 = cfg.base_y;
    for lane in 0..cfg.lanes {
        let x = cfg.lane_x(lane);
        gizmos.line_2d(
            Vec2::new(x, y0),
            Vec2::new(x, y1),
            Color::srgb(0.18, 0.18, 0.22),
        );
    }
    // Field border.
    let spread = cfg.lane_spread + 4.0;
    gizmos.rect_2d(
        Vec2::new(0.0, 0.0),
        Vec2::new(spread * 2.0, cfg.base_y * 2.0),
        Color::srgb(0.25, 0.25, 0.30),
    );

    // Base health bars (above each base square).
    for player in [0u8, 1u8] {
        let pos = sim_to_view(cfg.base_pos(player));
        let cur = battle.state.base_hp(player).max(0) as f32;
        let max = cfg.base_hp.max(1) as f32;
        let dir = if player == 0 { -1.0 } else { 1.0 };
        health_bar(
            &mut gizmos,
            pos,
            dir * 8.0,
            12.0,
            cur / max,
            team_color(player),
        );
    }

    // Deployed units: a disc scaled by HP, tinted by champion type, with a HP bar.
    for champ in &battle.state.world.champions {
        if battle.state.is_base(champ.id) || !champ.alive {
            continue;
        }
        let pos = sim_to_view(champ.pos);
        let max = champ.health.max.max(1) as f32;
        let frac = (champ.health.current.max(0) as f32 / max).clamp(0.0, 1.0);
        // No faction field exists on the roster yet; tint by champion type (max HP)
        // so Sentinel/Vanguard/Pyromancer read differently on the same team.
        let tint = champion_tint(&format!("hp{}", champ.health.max));
        let color = blend(team_color(champ.team), tint, 0.3);
        let radius = UNIT_RADIUS * (0.55 + 0.45 * frac);
        gizmos.circle_2d(pos, radius, color);
        // Inner dot marks the team clearly even when tinted.
        gizmos.circle_2d(pos, radius * 0.4, team_color(champ.team));
        health_bar(&mut gizmos, pos, UNIT_RADIUS + 1.2, UNIT_RADIUS * 2.0, frac, color);
    }

    // Selection outline around the chosen hand card.
    if let Some(idx) = selection.card {
        let n = battle.state.decks[0].len();
        let cx = card_center_x(idx, n);
        gizmos.rect_2d(
            Vec2::new(cx, CARD_ROW_Y),
            Vec2::new(CARD_W + 1.5, CARD_H + 1.5),
            Color::srgb(1.0, 0.9, 0.2),
        );
    }
}
