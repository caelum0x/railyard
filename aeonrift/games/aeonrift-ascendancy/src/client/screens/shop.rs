//! The shop / pack-opening screen (scaffold).
//!
//! Presents the packs from `content/cards/packs` and opens them with the deterministic,
//! seeded opener already implemented in [`crate::cards::pack`]. This is a compiling
//! scaffold: the plugin seam is in place; the pack shelf + reveal animation are pending.

use bevy::prelude::*;

/// Plugin for the shop screen.
pub struct ShopScreenPlugin;

impl Plugin for ShopScreenPlugin {
    fn build(&self, _app: &mut App) {
        // TODO: list packs (crate::cards::load_pack) and reveal opened cards
        // (crate::cards::open_pack) into the player's Collection.
    }
}
