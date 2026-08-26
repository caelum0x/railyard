//! The HUD: a 2D camera plus persistent sprite/text entities for the hand of cards,
//! the elixir bar, the status message, and the winner banner. These have fixed counts
//! so they are spawned once and merely re-styled each frame from the match state —
//! all values (elixir current/max, card names/costs) are read from the `cards` core.

use bevy::camera::{Hdr, ScalingMode};
use bevy::core_pipeline::tonemapping::Tonemapping;
use bevy::post_process::bloom::Bloom;
use bevy::prelude::*;

use super::game::{
    blend, new_battle, team_color, Battle, RunOptions, Selection, StatusMessage, BASE_SIZE,
    CAMERA_Y, CARD_H, CARD_ROW_Y, CARD_W, ELIXIR_Y, HUMAN, PIP_GAP, PIP_H, PIP_W, VIEW_HEIGHT,
};
use super::input::card_center_x;

// ---- Marker components ------------------------------------------------------
#[derive(Component)]
pub struct CardSprite(pub usize);
#[derive(Component)]
pub struct CardLabel(pub usize);
#[derive(Component)]
pub struct BaseSprite(pub u8);
#[derive(Component)]
pub struct ElixirPip(pub u32);
#[derive(Component)]
pub struct ElixirLabel;
#[derive(Component)]
pub struct MessageLabel;
#[derive(Component)]
pub struct WinnerLabel;

fn font(size: f32) -> TextFont {
    TextFont {
        font_size: FontSize::Px(size),
        ..default()
    }
}

/// Spawn the camera, the match resource, and every persistent HUD entity.
pub fn setup(mut commands: Commands, options: Res<RunOptions>, asset_server: Res<AssetServer>) {
    let battle = new_battle();

    // A fixed-height orthographic camera: the same vertical slice of the world is
    // visible on any screen aspect (portrait phone or desktop window).
    commands.spawn((
        Camera2d,
        // HDR (the `Hdr` marker) is required for bloom: the VFX bursts use over-bright
        // (>1.0) colors so the bloom pass blooms them into a readable glow.
        Hdr,
        Tonemapping::TonyMcMapface,
        Bloom {
            intensity: 0.45,
            ..Bloom::default()
        },
        Projection::from(OrthographicProjection {
            scaling_mode: ScalingMode::FixedVertical {
                viewport_height: VIEW_HEIGHT,
            },
            ..OrthographicProjection::default_2d()
        }),
        Transform::from_xyz(0.0, CAMERA_Y, 0.0),
    ));

    // Bases (towers): fixed team-colored squares; HP is drawn as a gizmo bar.
    for player in [0u8, 1u8] {
        let pos = battle.state.config.base_pos(player);
        commands.spawn((
            Sprite::from_color(team_color(player), Vec2::splat(BASE_SIZE)),
            Transform::from_xyz(pos.x, pos.y, 0.0),
            BaseSprite(player),
        ));
    }

    // Elixir pips (a Clash-style pool bar) + a numeric label.
    let max = battle.state.config.max_elixir;
    let pip_step = PIP_W + PIP_GAP;
    let pip_total = pip_step * max as f32 - PIP_GAP;
    let pip_start = -pip_total / 2.0 + PIP_W / 2.0;
    for i in 0..max {
        commands.spawn((
            Sprite::from_color(Color::srgb(0.2, 0.2, 0.25), Vec2::new(PIP_W, PIP_H)),
            Transform::from_xyz(pip_start + i as f32 * pip_step, ELIXIR_Y, 0.5),
            ElixirPip(i),
        ));
    }
    commands.spawn((
        Text2d::new("Elixir"),
        font(6.0),
        TextColor(Color::srgb(0.9, 0.7, 1.0)),
        Transform::from_xyz(0.0, ELIXIR_Y + 5.0, 1.0),
        ElixirLabel,
    ));

    // Hand cards: a background sprite + a name/cost label per deck slot.
    let deck = &battle.state.decks[HUMAN as usize];
    let n = deck.len();
    for (i, card) in deck.iter().enumerate() {
        let cx = card_center_x(i, n);
        // The hand card FACE is the real finished PNG (`assets/cards/final-v2/<slug>.png`)
        // — the played card is the pictured card. `update_hand` tints this sprite to
        // show affordability/selection; a white tint shows the art unmodified.
        let face: Handle<Image> = super::game::DECK_SLUGS
            .get(i)
            .map(|slug| asset_server.load(super::game::face_asset_path(slug)))
            .unwrap_or_default();
        commands.spawn((
            Sprite {
                image: face,
                custom_size: Some(Vec2::new(CARD_W, CARD_H)),
                ..default()
            },
            Transform::from_xyz(cx, CARD_ROW_Y, 0.5),
            CardSprite(i),
        ));
        // The elixir cost badge sits just below the face.
        commands.spawn((
            Text2d::new(format!("{}  ⬦{}", card.name(), card.meta.cost)),
            font(4.5),
            TextColor(Color::WHITE),
            TextLayout::justify(Justify::Center),
            Transform::from_xyz(cx, CARD_ROW_Y - CARD_H / 2.0 - 2.5, 1.0),
            CardLabel(i),
        ));
    }

    // Status message + winner banner (empty until needed).
    commands.spawn((
        Text2d::new(""),
        font(6.5),
        TextColor(Color::srgb(1.0, 0.85, 0.3)),
        Transform::from_xyz(0.0, -76.0, 2.0),
        MessageLabel,
    ));
    commands.spawn((
        Text2d::new(""),
        font(12.0),
        TextColor(Color::WHITE),
        Transform::from_xyz(0.0, 0.0, 3.0),
        WinnerLabel,
    ));

    commands.insert_resource(battle);
    let _ = options; // options consumed by main() to choose schedules
}

/// Recolor hand cards: affordable cards are bright; the selected one is highlighted;
/// unaffordable cards are dimmed. Reads cost + elixir straight from the core.
pub fn update_hand(
    battle: Res<Battle>,
    selection: Res<Selection>,
    mut cards: Query<(&CardSprite, &mut Sprite)>,
) {
    let p = HUMAN as usize;
    let elixir = battle.state.elixir[p];
    let deck = &battle.state.decks[p];
    for (marker, mut sprite) in &mut cards {
        let Some(card) = deck.get(marker.0) else {
            continue;
        };
        let affordable = card.meta.cost <= elixir;
        let selected = selection.card == Some(marker.0);
        // Tint multiplies the card-face texture: warm highlight when selected, full
        // art when affordable, dimmed when it can't be paid for.
        sprite.color = if selected {
            Color::srgb(1.0, 0.95, 0.55)
        } else if affordable {
            Color::WHITE
        } else {
            Color::srgb(0.45, 0.45, 0.5)
        };
    }
}

/// Fill elixir pips up to the current amount and update the numeric label.
pub fn update_elixir(
    battle: Res<Battle>,
    mut pips: Query<(&ElixirPip, &mut Sprite)>,
    mut label: Query<&mut Text2d, With<ElixirLabel>>,
) {
    let cur = battle.state.elixir[HUMAN as usize];
    let max = battle.state.config.max_elixir;
    for (pip, mut sprite) in &mut pips {
        sprite.color = if pip.0 < cur {
            Color::srgb(0.85, 0.25, 0.95)
        } else {
            Color::srgb(0.18, 0.18, 0.22)
        };
    }
    if let Ok(mut text) = label.single_mut() {
        text.0 = format!("Elixir  {cur}/{max}");
    }
}

/// Recolor bases (dim as they lose HP) so tower damage reads at a glance.
pub fn update_bases(battle: Res<Battle>, mut bases: Query<(&BaseSprite, &mut Sprite)>) {
    for (marker, mut sprite) in &mut bases {
        let frac = base_hp_fraction(&battle, marker.0);
        sprite.color = blend(
            Color::srgb(0.1, 0.1, 0.1),
            team_color(marker.0),
            frac.max(0.15),
        );
    }
}

/// HP fraction (0..=1) of a player's base, from the core's `MatchState`.
pub fn base_hp_fraction(battle: &Battle, player: u8) -> f32 {
    let cur = battle.state.base_hp(player) as f32;
    let max = battle.state.config.base_hp as f32;
    if max <= 0.0 {
        0.0
    } else {
        (cur / max).clamp(0.0, 1.0)
    }
}

/// Tick down and display the transient status message.
pub fn update_message(
    time: Res<Time>,
    mut status: ResMut<StatusMessage>,
    mut label: Query<&mut Text2d, With<MessageLabel>>,
) {
    if status.ttl > 0.0 {
        status.ttl -= time.delta_secs();
    }
    if let Ok(mut text) = label.single_mut() {
        text.0 = if status.ttl > 0.0 {
            status.text.clone()
        } else {
            String::new()
        };
    }
}

/// Show the winner banner once the core reports an outcome.
pub fn update_winner(battle: Res<Battle>, mut label: Query<&mut Text2d, With<WinnerLabel>>) {
    let Ok(mut text) = label.single_mut() else {
        return;
    };
    text.0 = match battle.state.outcome {
        Some(crate::Outcome::Winner(p)) if p == HUMAN => "VICTORY".to_string(),
        Some(crate::Outcome::Winner(_)) => "DEFEAT".to_string(),
        Some(crate::Outcome::Draw) => "DRAW".to_string(),
        None => String::new(),
    };
}
