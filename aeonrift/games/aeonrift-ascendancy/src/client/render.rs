//! Battlefield rendering: textured board units + gizmo field furniture.
//!
//! Board units are drawn as SPRITES carrying each champion's real finished card face
//! (`assets/cards/final-v2/<slug>.png`) — the same "played card is the pictured card"
//! seam the hand already uses, now on the field. [`sync_unit_sprites`] keeps one
//! retained sprite entity per live deployed unit (spawns on deploy, follows the sim
//! position each frame, despawns on death). Everything else — lane guides, the field
//! border, base HP bars, the faction-tinted unit frame + unit HP bar, and the hand
//! selection outline — is drawn immediately with gizmos in [`draw_field`] /
//! [`draw_unit_overlays`]. Nothing here mutates match state; it is a pure view.

use bevy::prelude::*;

use super::game::{
    face_asset_path, sim_to_view, team_color, unit_frame_color, Battle, Selection, CARD_H,
    CARD_ROW_Y, CARD_W,
};
use super::input::card_center_x;
use sim::EntityId;

/// On-field footprint of a board unit's face sprite (world units). Sized to read at
/// board scale without swamping the lanes.
pub const UNIT_SPRITE_W: f32 = 5.6;
pub const UNIT_SPRITE_H: f32 = 7.2;

/// A retained board-unit sprite, tagged with the sim entity id it mirrors.
#[derive(Component)]
pub struct UnitSprite(pub EntityId);

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

/// Spawn/update/despawn one face sprite per live deployed unit so the board shows the
/// REAL champion card faces instead of anonymous discs. Retained-mode: sprites persist
/// across frames and merely follow the sim, so textures are loaded once per unit.
pub fn sync_unit_sprites(
    mut commands: Commands,
    battle: Res<Battle>,
    asset_server: Res<AssetServer>,
    mut sprites: Query<(Entity, &UnitSprite, &mut Transform)>,
) {
    // 1) Update existing sprites; collect which unit ids currently have one, and drop
    //    sprites whose unit has died or left the world.
    let mut have: std::collections::HashSet<EntityId> = std::collections::HashSet::new();
    for (entity, marker, mut tf) in &mut sprites {
        let live = battle
            .state
            .world
            .champion(marker.0)
            .filter(|c| c.alive && !battle.state.is_base(c.id));
        match live {
            Some(champ) => {
                let p = sim_to_view(champ.pos);
                tf.translation.x = p.x;
                tf.translation.y = p.y;
                have.insert(marker.0);
            }
            None => commands.entity(entity).despawn(),
        }
    }

    // 2) Spawn a face sprite for any live deployed unit that does not have one yet.
    for champ in &battle.state.world.champions {
        if battle.state.is_base(champ.id) || !champ.alive || have.contains(&champ.id) {
            continue;
        }
        let Some(slug) = battle.slug_of(champ.id) else {
            continue; // unknown slug: no face to show (should not happen for deploys)
        };
        let face: Handle<Image> = asset_server.load(face_asset_path(slug));
        let p = sim_to_view(champ.pos);
        commands.spawn((
            Sprite {
                image: face,
                custom_size: Some(Vec2::new(UNIT_SPRITE_W, UNIT_SPRITE_H)),
                ..default()
            },
            Transform::from_xyz(p.x, p.y, 0.6),
            UnitSprite(champ.id),
        ));
    }
}

/// Draw the lanes, the field border, base HP bars, and the hand selection outline.
pub fn draw_field(mut gizmos: Gizmos, battle: Res<Battle>, selection: Res<Selection>) {
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

/// Overlay each live board unit with a faction-tinted frame and an HP bar. Drawn with
/// gizmos on top of the face sprite so the team side + champion identity + health all
/// read at a glance at board scale.
pub fn draw_unit_overlays(mut gizmos: Gizmos, battle: Res<Battle>) {
    for champ in &battle.state.world.champions {
        if battle.state.is_base(champ.id) || !champ.alive {
            continue;
        }
        let pos = sim_to_view(champ.pos);
        let max = champ.health.max.max(1) as f32;
        let frac = (champ.health.current.max(0) as f32 / max).clamp(0.0, 1.0);
        let slug = battle.slug_of(champ.id).unwrap_or("unknown");
        let frame = unit_frame_color(champ.team, slug);
        // Faction/team frame around the face sprite.
        gizmos.rect_2d(
            pos,
            Vec2::new(UNIT_SPRITE_W + 0.7, UNIT_SPRITE_H + 0.7),
            frame,
        );
        // HP bar just above the frame, colored by the same accent.
        health_bar(
            &mut gizmos,
            pos,
            UNIT_SPRITE_H / 2.0 + 1.3,
            UNIT_SPRITE_W,
            frac,
            frame,
        );
    }
}
