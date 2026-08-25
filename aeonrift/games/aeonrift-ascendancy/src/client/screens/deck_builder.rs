//! The deck-builder screen (scaffold).
//!
//! Lets a player assemble / edit a deck against the deck-building ruleset already
//! implemented in [`crate::deck`] (8 cards, no duplicates, all from the roster) and
//! choose from the 12 preset decks in `content/cards/decks`. This is a compiling
//! scaffold: the plugin seam is in place; the card grid + validation UI are pending.

use bevy::prelude::*;

/// Plugin for the deck-builder screen.
pub struct DeckBuilderScreenPlugin;

impl Plugin for DeckBuilderScreenPlugin {
    fn build(&self, _app: &mut App) {
        // TODO: render the collection grid + editable deck slots; validate on save via
        // crate::deck::validate_deck_slugs. Preset decks come from
        // crate::deck::load_default_presets().
    }
}
