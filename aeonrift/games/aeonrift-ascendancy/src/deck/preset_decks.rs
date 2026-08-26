//! The 12 preset decks, loaded from `content/cards/decks/*.ron`.
//!
//! Each `DeckDef` names a champion slug per slot; loading resolves those slugs against
//! the shared roster into ready-to-play [`Card`]s, validating the deck ([`DECK_SIZE`]
//! cards, no duplicates, all from the roster) as it goes. Decks are returned in sorted
//! filename order so the set is deterministic.

use std::path::Path;

use serde::Deserialize;

use crate::cards::card::Card;
use crate::cards::card_pool::load_card_by_slug;
use crate::cards::CardError;

use super::validation::{validate_deck_slugs, DeckError};

/// A preset deck as authored in RON: a name and a list of champion slugs.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct DeckDef {
    pub name: String,
    pub cards: Vec<String>,
}

/// A loaded, validated preset deck: its name, slugs, and resolved cards.
#[derive(Clone, Debug, PartialEq)]
pub struct PresetDeck {
    pub name: String,
    pub slugs: Vec<String>,
    pub cards: Vec<Card>,
}

/// Errors from loading a preset deck.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PresetError {
    /// A content IO / parse error.
    Content(CardError),
    /// The deck failed the deck-building ruleset.
    Invalid(DeckError),
}

impl std::fmt::Display for PresetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PresetError::Content(e) => write!(f, "{e}"),
            PresetError::Invalid(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for PresetError {}

impl From<CardError> for PresetError {
    fn from(e: CardError) -> Self {
        PresetError::Content(e)
    }
}
impl From<DeckError> for PresetError {
    fn from(e: DeckError) -> Self {
        PresetError::Invalid(e)
    }
}

/// Parse a deck definition from RON text.
pub fn parse_deck(ron_text: &str) -> Result<DeckDef, CardError> {
    ron::from_str::<DeckDef>(ron_text).map_err(|e| CardError::Parse(e.to_string()))
}

/// Resolve + validate a `DeckDef` into a [`PresetDeck`] against `champions_dir`.
pub fn resolve_deck(
    def: DeckDef,
    champions_dir: impl AsRef<Path>,
) -> Result<PresetDeck, PresetError> {
    let dir = champions_dir.as_ref();
    validate_deck_slugs(&def.cards, dir)?;
    let cards = def
        .cards
        .iter()
        .map(|slug| load_card_by_slug(dir, slug))
        .collect::<Result<Vec<Card>, CardError>>()?;
    Ok(PresetDeck {
        name: def.name,
        slugs: def.cards,
        cards,
    })
}

/// Load a single preset deck file, resolving its cards against `champions_dir`.
pub fn load_deck_file(
    path: impl AsRef<Path>,
    champions_dir: impl AsRef<Path>,
) -> Result<PresetDeck, PresetError> {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .map_err(|e| CardError::Io(format!("{}: {e}", path.display())))?;
    let def = parse_deck(&text)?;
    resolve_deck(def, champions_dir)
}

/// Load every preset deck from `<cards_dir>/decks/*.ron`, resolved against
/// `champions_dir`, in sorted filename order.
pub fn load_preset_decks(
    cards_dir: impl AsRef<Path>,
    champions_dir: impl AsRef<Path>,
) -> Result<Vec<PresetDeck>, PresetError> {
    let decks_dir = cards_dir.as_ref().join("decks");
    let read = std::fs::read_dir(&decks_dir)
        .map_err(|e| CardError::Io(format!("{}: {e}", decks_dir.display())))?;

    let mut paths: Vec<std::path::PathBuf> = Vec::new();
    for item in read {
        let item = item.map_err(|e| CardError::Io(e.to_string()))?;
        let path = item.path();
        if path.extension().and_then(|e| e.to_str()) == Some("ron") {
            paths.push(path);
        }
    }
    paths.sort();

    let champions_dir = champions_dir.as_ref();
    paths
        .into_iter()
        .map(|p| load_deck_file(p, champions_dir))
        .collect()
}

/// Load the preset decks from the committed content directories (default paths).
pub fn load_default_presets() -> Result<Vec<PresetDeck>, PresetError> {
    load_preset_decks(crate::CONTENT_CARDS_DIR, crate::CONTENT_CHAMPIONS_DIR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_preset_decks_load_and_validate() {
        let decks = load_default_presets().expect("preset decks load + validate");
        // The shipped content set grew past the original 12 curated decks (the
        // content-mega consolidation added faction/archetype variants). The contract
        // is "at least the 12 curated decks, and every shipped deck is legal".
        assert!(
            decks.len() >= 12,
            "expected at least 12 preset decks, found {}",
            decks.len()
        );
        for d in &decks {
            assert_eq!(d.cards.len(), super::super::validation::DECK_SIZE);
            assert!(!d.name.is_empty());
        }
    }
}
