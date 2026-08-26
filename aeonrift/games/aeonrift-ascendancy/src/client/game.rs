//! Game state resources, the deck, and the sim->view coordinate mapping.
//!
//! This module owns the seam to the deterministic core: the [`Battle`] resource
//! wraps a `crate::MatchState`. Nothing here re-implements a rule — decks are built
//! from the shared champion roster (`champions::load_str` on embedded specs) and the
//! match is advanced only through `crate::step_tick` / `crate::try_deploy` (see the
//! `tick` and `input` modules).

use std::collections::HashMap;

use bevy::prelude::*;

use crate::{Card, MatchConfig, MatchState};
use sim::EntityId;

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

/// A faction/ability-identity hue (0..360) derived deterministically from a champion
/// slug. The shared roster carries no faction field yet, so — exactly like
/// [`champion_tint`] — identity is hashed from the slug. This drives the board unit's
/// frame accent AND the color of its ability-cast VFX, so a champion reads the same in
/// both places (the "same card, same colour" seam).
pub fn faction_hue(slug: &str) -> f32 {
    let mut h: u32 = 2166136261;
    for b in slug.bytes() {
        h ^= b as u32;
        h = h.wrapping_mul(16777619);
    }
    (h % 360) as f32
}

/// The saturated identity color for a slug (used for the unit frame + as the VFX base).
pub fn faction_color(slug: &str) -> Color {
    Color::hsl(faction_hue(slug), 0.9, 0.58)
}

/// The frame accent around a board unit: the team side blended toward the champion's
/// faction color, so team (azure/crimson) and identity (hue) both read at a glance.
pub fn unit_frame_color(team: u8, slug: &str) -> Color {
    blend(team_color(team), faction_color(slug), 0.45)
}

/// The high-dynamic-range core color of a cast burst: the faction color pushed past
/// 1.0 in linear space so the camera's bloom makes it glow. `team` nudges the tone so
/// friendly casts lean azure and enemy casts lean crimson without losing identity.
pub fn vfx_color(team: u8, slug: &str) -> Color {
    let base = blend(faction_color(slug), team_color(team), 0.25).to_linear();
    // Over-drive the brightest channels so HDR + bloom produce a readable glow.
    const GAIN: f32 = 4.0;
    Color::linear_rgb(base.red * GAIN, base.green * GAIN, base.blue * GAIN)
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
    /// Deployed-unit entity id => champion slug, filled at deploy time. This is the
    /// client VIEW's copy of the recorder's `id => slug` map (see `rules::replay`): it
    /// lets a board unit render its REAL finished card face (`final-v2/<slug>.png`)
    /// instead of an anonymous disc. The rules core stays slug-free + deterministic.
    pub unit_slugs: HashMap<EntityId, String>,
}

impl Battle {
    /// Remember that the freshly deployed unit `id` was played from `slug`'s card.
    pub fn record_unit(&mut self, id: EntityId, slug: impl Into<String>) {
        self.unit_slugs.insert(id, slug.into());
    }

    /// The champion slug for a live unit id, if known.
    pub fn slug_of(&self, id: EntityId) -> Option<&str> {
        self.unit_slugs.get(&id).map(String::as_str)
    }
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

/// The file slugs of the deck, in the same order as [`build_deck`]. These key the
/// finished card faces (`assets/cards/final-v2/<slug>.png`) the hand renders — the
/// played-card === pictured-card seam, in the client's VIEW.
pub const DECK_SLUGS: [&str; 3] = ["sentinel", "vanguard", "pyromancer"];

/// The Bevy asset path (relative to the asset root, which `run_windowed` points at the
/// repo `assets/` dir) for a champion's finished card face.
pub fn face_asset_path(slug: &str) -> String {
    format!("cards/final-v2/{slug}.png")
}

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
        unit_slugs: HashMap::new(),
    }
}

/// The champion slug a given player's card index maps to. Both players field
/// [`build_deck`] (the same three-champion roster), so the slug is the deck slug at
/// that index — this is the VIEW's link from a deploy to a finished card face.
pub fn slug_for_card(card_index: usize) -> Option<&'static str> {
    DECK_SLUGS.get(card_index).copied()
}
