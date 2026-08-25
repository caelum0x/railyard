//! Deck validation — the deck-building ruleset.
//!
//! A legal deck is exactly [`DECK_SIZE`] cards, with no duplicate champion, every one
//! drawn from the shared roster (`content/champions`). Validation fails fast with an
//! explicit [`DeckError`]; nothing is silently dropped or truncated.

use std::path::Path;

use crate::cards::card_pool::load_card_by_slug;

/// Cards in a legal deck (Clash-style).
pub const DECK_SIZE: usize = 8;

/// Why a deck was rejected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeckError {
    /// The deck did not contain exactly [`DECK_SIZE`] cards.
    WrongSize { have: usize },
    /// A champion slug appeared more than once.
    Duplicate(String),
    /// A slug did not resolve to a roster champion.
    UnknownCard(String),
}

impl std::fmt::Display for DeckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeckError::WrongSize { have } => {
                write!(f, "deck must have {DECK_SIZE} cards, has {have}")
            }
            DeckError::Duplicate(s) => write!(f, "duplicate card in deck: {s}"),
            DeckError::UnknownCard(s) => write!(f, "unknown card (not in roster): {s}"),
        }
    }
}

impl std::error::Error for DeckError {}

/// Validate a list of champion slugs as a deck against `champions_dir`.
pub fn validate_deck_slugs(
    slugs: &[String],
    champions_dir: impl AsRef<Path>,
) -> Result<(), DeckError> {
    if slugs.len() != DECK_SIZE {
        return Err(DeckError::WrongSize { have: slugs.len() });
    }
    let dir = champions_dir.as_ref();
    let mut seen: Vec<&str> = Vec::with_capacity(slugs.len());
    for slug in slugs {
        if seen.contains(&slug.as_str()) {
            return Err(DeckError::Duplicate(slug.clone()));
        }
        seen.push(slug);
        // A card is "from the roster" iff its spec loads + validates.
        load_card_by_slug(dir, slug).map_err(|_| DeckError::UnknownCard(slug.clone()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrong_size_is_rejected() {
        let slugs = vec!["zeus".to_string(), "hades".to_string()];
        assert_eq!(
            validate_deck_slugs(&slugs, crate::CONTENT_CHAMPIONS_DIR),
            Err(DeckError::WrongSize { have: 2 })
        );
    }

    #[test]
    fn duplicates_are_rejected() {
        let slugs: Vec<String> = std::iter::repeat("zeus".to_string()).take(DECK_SIZE).collect();
        assert!(matches!(
            validate_deck_slugs(&slugs, crate::CONTENT_CHAMPIONS_DIR),
            Err(DeckError::Duplicate(_))
        ));
    }

    #[test]
    fn unknown_card_is_rejected() {
        let mut slugs: Vec<String> = vec![
            "zeus", "hades", "athena", "medusa", "achilles", "artemis", "mars",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        slugs.push("not-a-real-champion".to_string());
        assert!(matches!(
            validate_deck_slugs(&slugs, crate::CONTENT_CHAMPIONS_DIR),
            Err(DeckError::UnknownCard(_))
        ));
    }
}
