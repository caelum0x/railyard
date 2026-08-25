//! # `modes` — game mode configuration.
//!
//! Every mode is described by a [`ModeConfig`]: the deck-building rules it enforces,
//! whether it feeds ranked matchmaking, an optional PvE bot difficulty, and the
//! [`MatchConfig`] its matches run under. The three shipped modes each provide a
//! constructor:
//! - [`ladder::config`] — ranked ladder,
//! - [`draft::config`]  — draft (build a deck by picking),
//! - [`pve::config`]    — practice vs a bot.

pub mod draft;
pub mod ladder;
pub mod pve;

use crate::ai::deploy_bot::BotDifficulty;
use crate::deck::validation::DECK_SIZE;
use crate::rules::config::MatchConfig;

/// The playable modes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Ranked ladder with a player-built preset deck.
    Ladder,
    /// Draft: assemble a deck on the spot from offered cards.
    Draft,
    /// Practice against a deterministic bot.
    Pve,
}

/// A mode's rules: deck constraints, matchmaking, bot, and the underlying match rules.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ModeConfig {
    pub mode: Mode,
    /// Required deck size for this mode.
    pub deck_size: usize,
    /// Whether players bring a pre-built preset deck (`true`) or draft one (`false`).
    pub uses_preset_deck: bool,
    /// Whether results affect ranked matchmaking rating.
    pub ranked: bool,
    /// The bot difficulty, if the mode faces a bot (PvE).
    pub bot: Option<BotDifficulty>,
    /// The match rules matches in this mode run under.
    pub match_config: MatchConfig,
}

impl ModeConfig {
    /// The default config for a mode.
    pub fn for_mode(mode: Mode) -> Self {
        match mode {
            Mode::Ladder => ladder::config(),
            Mode::Draft => draft::config(),
            Mode::Pve => pve::config(BotDifficulty::Normal),
        }
    }
}

/// The shared default deck size across modes (Clash-style 8).
pub const STANDARD_DECK_SIZE: usize = DECK_SIZE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_mode_has_a_sane_config() {
        for mode in [Mode::Ladder, Mode::Draft, Mode::Pve] {
            let c = ModeConfig::for_mode(mode);
            assert_eq!(c.mode, mode);
            assert!(c.deck_size >= 1);
        }
        assert!(ModeConfig::for_mode(Mode::Ladder).ranked);
        assert!(!ModeConfig::for_mode(Mode::Pve).ranked);
        assert!(ModeConfig::for_mode(Mode::Pve).bot.is_some());
    }
}
