//! Client screens.
//!
//! The touch client is organised as a small set of screens: the [`menu`], the
//! [`deck_builder`], the live [`match`](r#match), and the [`shop`]. The in-match
//! screen is fully implemented in [`super::render`] + [`super::hud`] + [`super::input`]
//! (a thin VIEW over the deterministic core); the menu / deck-builder / shop screens
//! are compiling **scaffolds** that establish the plugin seams and will grow their UI
//! on top of the already-real rules (`crate::deck`, `crate::cards::pack`, etc.).
//!
//! Each screen is a Bevy [`Plugin`] so the app can compose them; [`AppScreen`] is the
//! resource a top-level router would switch on.

use bevy::prelude::*;

pub mod deck_builder;
pub mod collection;
pub mod r#match;
pub mod menu;
pub mod shop;

/// Which top-level screen the client is currently showing.
#[derive(Resource, Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum AppScreen {
    /// The main menu (play / decks / shop).
    #[default]
    Menu,
    /// The deck-building screen.
    DeckBuilder,
    /// Owned cards and copy counts.
    Collection,
    /// The live match.
    Match,
    /// The card shop / pack opening.
    Shop,
}
