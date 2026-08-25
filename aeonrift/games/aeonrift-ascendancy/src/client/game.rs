//! Game state resources, the deck, and the sim->view coordinate mapping.
//!
//! This module owns the seam to the deterministic core: the [`Battle`] resource
//! wraps a `crate::MatchState`. Nothing here re-implements a rule — decks are built
//! from the shared champion roster (`champions::load_str` on embedded specs) and the
//! match is advanced only through `crate::step_tick` / `crate::try_deploy` (see the
//! `tick` and `input` modules).

use bevy::prelude::*;

use crate::{Card, MatchConfig, MatchState};

/// The player controlled by the human on this device. Player 1 is the bot.
pub const HUMAN: u8 = 0;
pub const BOT: u8 = 1;

/// Ticks between bot deploy attempts (~1s at 30 Hz), reusing the core's bot policy.
pub const BOT_INTERVAL: u64 = 30;

// ---- View layout (world units; sim coordinates are rendered 1:1) ------------
/// The camera keeps this many world units visible vertically, regardless of screen
/// aspect ([`bevy::camera::ScalingMode::FixedVertical`]). Sized to show both bases
/// (y = +/-40) plus the HUD strip beneath the human's base.
pub const VIEW_HEIGHT: f32 = 138.0;
/// Camera center, shifted down so the HUD (negative y) is on screen.
pub const CAMERA_Y: f32 = -14.0;

/// A deployed unit is drawn as a disc of this radius at full HP (scaled by HP).
pub const UNIT_RADIUS: f32 = 2.6;
/// Base (tower) drawn as a square of this side.
pub const BASE_SIZE: f32 = 10.0;

/// HUD hand card size and row position.
pub const CARD_W: f32 = 12.5;
pub const CARD_H: f32 = 17.0;
pub const CARD_ROW_Y: f32 = -66.0;
pub const CARD_GAP: f32 = 1.8;

/// Elixir pip row.
pub const ELIXIR_Y: f32 = -50.0;
pub const PIP_W: f32 = 2.2;
pub const PIP_H: f32 = 3.4;
pub const PIP_GAP: f32 = 0.8;

/// The battlefield's lower edge in view space: taps below this are HUD, above are
/// the field (used to distinguish "select a card" from "deploy into a lane").
pub const FIELD_BOTTOM: f32 = -44.0;

/// Team colors: the human (team 0) is azure, the bot (team 1) is crimson.
pub fn team_color(team: u8) -> Color {
    if team == HUMAN {
        Color::srgb(0.30, 0.60, 1.00)
    } else {
        Color::srgb(1.00, 0.35, 0.32)
    }
}

/// A per-champion accent hue used to *tint* a unit on top of its team color, so
/// different champions are visually distinct. The shared roster carries no faction
/// field today (`roster_meta` does not exist yet), so identity is derived
/// deterministically from the champion name — a stand-in for a future faction tint.
pub fn champion_tint(name: &str) -> Color {
    let mut h: u32 = 2166136261;
    for b in name.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    let hue = (h % 360) as f32;
    Color::hsl(hue, 0.85, 0.6)
}

/// Blend two colors in linear RGB by `t` (0 => a, 1 => b).
pub fn blend(a: Color, b: Color, t: f32) -> Color {
    let a = a.to_linear();
    let b = b.to_linear();
    Color::linear_rgb(
        a.red + (b.red - a.red) * t,
        a.green + (b.green - a.green) * t,
        a.blue + (b.blue - a.blue) * t,
    )
}

/// Map a sim-world position to a Bevy world position (1:1; y already points "up").
pub fn sim_to_view(p: sim::Vec2) -> Vec2 {
    Vec2::new(p.x, p.y)
}

// ---- Resources --------------------------------------------------------------

/// The whole match: a thin wrapper over the deterministic core's `MatchState`.
#[derive(Resource)]
pub struct Battle {
    pub state: MatchState,
    pub bot_interval: u64,
}

/// The human's current card selection (index into their deck), if any.
#[derive(Resource, Default)]
pub struct Selection {
    pub card: Option<usize>,
}

/// A transient on-screen status line (e.g. a rejected deploy reason).
#[derive(Resource, Default)]
pub struct StatusMessage {
    pub text: String,
    /// Seconds remaining before the message fades.
    pub ttl: f32,
}

impl StatusMessage {
    pub fn set(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.ttl = 2.5;
    }
}

/// Runtime options resolved from CLI/env (headless smoke vs. windowed play).
#[derive(Resource, Clone, Copy)]
pub struct RunOptions {
    pub headless: bool,
    /// In headless mode, stop after this many stepped ticks even if undecided.
    pub headless_max_ticks: u64,
}

// ---- Deck construction (reuses the shared roster, embedded at compile time) --

// Champion specs are embedded so the deck loads identically on desktop and mobile
// (no runtime file IO / asset bundling needed to prove the seam).
const SENTINEL_RON: &str = include_str!("../../../../content/champions/sentinel.ron");
const VANGUARD_RON: &str = include_str!("../../../../content/champions/vanguard.ron");
const PYROMANCER_RON: &str = include_str!("../../../../content/champions/pyromancer.ron");

/// Build a deck from the shared roster. Each spec is parsed + fully validated by the
/// `champions` DSL, then turned into a `crate::Card` (which derives the elixir cost).
/// Panics only if an embedded roster spec is malformed — a build-time invariant.
pub fn build_deck() -> Vec<Card> {
    [SENTINEL_RON, VANGUARD_RON, PYROMANCER_RON]
        .iter()
        .map(|ron| {
            let spec = champions::load_str(ron).expect("embedded roster spec must validate");
            Card::from_spec(spec)
        })
        .collect()
}

/// Create the initial match: both players field the same three-champion roster.
pub fn new_battle() -> Battle {
    let config = MatchConfig::default();
    Battle {
        state: MatchState::new(config, build_deck(), build_deck()),
        bot_interval: BOT_INTERVAL,
    }
}
