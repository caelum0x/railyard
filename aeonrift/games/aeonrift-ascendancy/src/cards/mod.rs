//! # `cards` — the card layer over the shared champion roster.
//!
//! A card is a champion (see [`card`]): the reuse seam that turns a
//! `champions::ChampionSpec` into a deployable unit with an elixir cost. On top of
//! that this module adds the collectible economy:
//! - [`rarity`]    — rarity tiers loaded from `content/cards/rarities.ron`,
//! - [`card_pool`] — the whole roster as a catalogue + a player [`Collection`],
//! - [`pack`]      — deterministic, seeded pack opening.

pub mod card;
pub mod card_pool;
pub mod pack;
pub mod rarity;

pub use card::{elixir_cost, Card, CardMeta, MAX_COST, MIN_COST};
pub use card_pool::{load_card_by_slug, CardPool, Collection, PoolCard};
pub use pack::{load_pack, open_pack, parse_pack, PackDef};
pub use rarity::{default_rarity_table, load_rarity_table, RarityTable, RarityTier};

/// Errors from loading or parsing card content.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardError {
    /// A filesystem error (missing / unreadable content file).
    Io(String),
    /// RON parse error (malformed content).
    Parse(String),
    /// Semantic content error (e.g. a champion spec that fails DSL validation).
    Content(String),
}

impl std::fmt::Display for CardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CardError::Io(m) => write!(f, "card content io error: {m}"),
            CardError::Parse(m) => write!(f, "card content parse error: {m}"),
            CardError::Content(m) => write!(f, "card content error: {m}"),
        }
    }
}

impl std::error::Error for CardError {}
