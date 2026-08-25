//! The main menu screen (scaffold).
//!
//! Entry point of the client UI: Play (ladder / pve), Decks, and Shop. This is a
//! compiling scaffold — the plugin seam is in place; the buttons and navigation to the
//! other screens are not yet wired.

use bevy::prelude::*;

use super::AppScreen;

/// Plugin for the main menu screen.
pub struct MenuScreenPlugin;

impl Plugin for MenuScreenPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AppScreen>();
        // TODO: spawn menu buttons and route to DeckBuilder / Match / Shop.
    }
}
