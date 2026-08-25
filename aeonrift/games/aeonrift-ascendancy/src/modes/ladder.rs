//! Ranked ladder mode.
//!
//! Players bring a pre-built preset deck ([`STANDARD_DECK_SIZE`] cards) and results
//! move their rating. [`matchmaking_band`] gives the initial rating window the server
//! should search when queueing a ladder player (see [`crate::server::matchmaking`]).

use super::{Mode, ModeConfig, STANDARD_DECK_SIZE};
use crate::rules::config::MatchConfig;

/// The initial +/- rating window ladder matchmaking searches within.
pub const DEFAULT_RATING_BAND: i32 = 100;

/// The ranked ladder mode config.
pub fn config() -> ModeConfig {
    ModeConfig {
        mode: Mode::Ladder,
        deck_size: STANDARD_DECK_SIZE,
        uses_preset_deck: true,
        ranked: true,
        bot: None,
        match_config: MatchConfig::default(),
    }
}

/// The rating window to search for an opponent near `rating`, widening by `attempts`
/// already made (so a long-waiting player matches more loosely).
pub fn matchmaking_band(rating: i32, attempts: u32) -> (i32, i32) {
    let width = DEFAULT_RATING_BAND + (attempts as i32) * 50;
    (rating - width, rating + width)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ladder_is_ranked_preset() {
        let c = config();
        assert!(c.ranked && c.uses_preset_deck && c.bot.is_none());
        assert_eq!(c.deck_size, STANDARD_DECK_SIZE);
    }

    #[test]
    fn band_widens_with_attempts() {
        let (lo0, hi0) = matchmaking_band(1000, 0);
        let (lo1, hi1) = matchmaking_band(1000, 3);
        assert!(hi1 - lo1 > hi0 - lo0);
        assert!(lo0 < 1000 && hi0 > 1000);
    }
}
