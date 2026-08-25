//! # `deck` — deck building over the roster.
//!
//! - [`validation`]    — the deck-building ruleset ([`DECK_SIZE`] cards, no duplicates,
//!   all from the roster).
//! - [`preset_decks`]  — the 12 curated decks in `content/cards/decks/*.ron`, resolved
//!   into ready-to-play cards.
//!
//! Note: this is the deck *authoring / building* layer (a static list of 8 cards). The
//! in-match card cycling (draw pile + rotating hand) lives in [`crate::rules::deck`]
//! and [`crate::rules::hand`].

pub mod preset_decks;
pub mod validation;

pub use preset_decks::{
    load_default_presets, load_deck_file, load_preset_decks, parse_deck, resolve_deck, DeckDef,
    PresetDeck, PresetError,
};
pub use validation::{validate_deck_slugs, DeckError, DECK_SIZE};
