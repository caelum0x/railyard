//! (e) PRESET-DECK gate — the 12 curated decks load + validate from content.
//!
//! Every deck in `content/cards/decks/*.ron` must resolve against the shared roster
//! and satisfy the deck-building ruleset (exactly 8 cards, no duplicates, all real
//! champions). This is the content contract that keeps the shipped decks legal.

use cards::deck::{load_default_presets, validate_deck_slugs, DeckError, DECK_SIZE};

#[test]
fn all_twelve_preset_decks_load_and_are_legal() {
    let decks = load_default_presets().expect("all preset decks load + validate");
    // The content-mega consolidation grew the shipped set beyond the original 12
    // curated decks; the contract is "at least the 12 curated decks, all legal".
    assert!(
        decks.len() >= 12,
        "the roster ships at least 12 preset decks, found {}",
        decks.len()
    );

    for d in &decks {
        assert_eq!(d.cards.len(), DECK_SIZE, "{}: must have 8 cards", d.name);
        assert_eq!(d.slugs.len(), DECK_SIZE);
        // Every resolved card carries a champion spec + a derived cost.
        for c in &d.cards {
            assert!(!c.name().is_empty());
            assert!((1..=10).contains(&c.meta.cost));
        }
    }
}

#[test]
fn preset_deck_names_are_unique() {
    let decks = load_default_presets().expect("presets load");
    let mut names: Vec<&str> = decks.iter().map(|d| d.name.as_str()).collect();
    names.sort_unstable();
    let before = names.len();
    names.dedup();
    assert_eq!(before, names.len(), "preset deck names must be unique");
}

#[test]
fn deck_validation_rejects_illegal_decks() {
    // Too small.
    assert_eq!(
        validate_deck_slugs(&["zeus".into()], cards::CONTENT_CHAMPIONS_DIR),
        Err(DeckError::WrongSize { have: 1 })
    );
    // A slug not in the roster.
    let bad: Vec<String> = [
        "zeus",
        "hades",
        "athena",
        "medusa",
        "achilles",
        "artemis",
        "mars",
        "ghost-not-real",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert!(matches!(
        validate_deck_slugs(&bad, cards::CONTENT_CHAMPIONS_DIR),
        Err(DeckError::UnknownCard(_))
    ));
}
