//! PvE mode: practice against a deterministic bot.
//!
//! Config plus a headless driver ([`simulate`]) that plays a full match with each side
//! driven by the shared [`crate::ai::deploy_bot`] policy at a chosen difficulty. This
//! is the reusable PvE engine (also handy in tests): a real, deterministic match
//! between two bot difficulties.

use super::{Mode, ModeConfig, STANDARD_DECK_SIZE};
use crate::ai::deploy_bot::{bot_deploy_for, BotDifficulty};
use crate::cards::card::Card;
use crate::rules::config::MatchConfig;
use crate::rules::match_state::{MatchState, Outcome};
use crate::rules::runner::{step_tick, DEFAULT_DEPLOY_INTERVAL};

/// The PvE mode config at `difficulty`.
pub fn config(difficulty: BotDifficulty) -> ModeConfig {
    ModeConfig {
        mode: Mode::Pve,
        deck_size: STANDARD_DECK_SIZE,
        uses_preset_deck: true,
        ranked: false,
        bot: Some(difficulty),
        match_config: MatchConfig::default(),
    }
}

/// Play a full deterministic match with player 0 and player 1 each driven by a bot at
/// the given difficulty (the PvE opponent engine; player 0 stands in for the human in
/// a preview / test). Runs to a decisive outcome or the tick cap (then a draw).
pub fn simulate(
    config: MatchConfig,
    deck0: Vec<Card>,
    deck1: Vec<Card>,
    diff0: BotDifficulty,
    diff1: BotDifficulty,
    interval: u64,
) -> MatchState {
    let mut m = MatchState::new(config, deck0, deck1);
    while !m.is_over() && m.world.tick < config.max_ticks {
        let t = m.world.tick;
        let mut deploys = Vec::new();
        if t != 0 && interval != 0 && t.is_multiple_of(interval) {
            if let Some(d) = bot_deploy_for(&m, 0, diff0) {
                deploys.push(d);
            }
            if let Some(d) = bot_deploy_for(&m, 1, diff1) {
                deploys.push(d);
            }
        }
        step_tick(&mut m, &deploys);
    }
    if !m.is_over() {
        m.outcome = Some(Outcome::Draw);
    }
    m
}

/// Convenience: [`simulate`] at the default deploy interval.
pub fn simulate_default(
    config: MatchConfig,
    deck0: Vec<Card>,
    deck1: Vec<Card>,
    diff0: BotDifficulty,
    diff1: BotDifficulty,
) -> MatchState {
    simulate(config, deck0, deck1, diff0, diff1, DEFAULT_DEPLOY_INTERVAL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cards::card_pool::load_card_by_slug;

    fn card(slug: &str) -> Card {
        load_card_by_slug(crate::CONTENT_CHAMPIONS_DIR, slug).expect("card loads")
    }

    #[test]
    fn pve_config_faces_a_bot() {
        let c = config(BotDifficulty::Hard);
        assert_eq!(c.mode, Mode::Pve);
        assert_eq!(c.bot, Some(BotDifficulty::Hard));
        assert!(!c.ranked);
    }

    #[test]
    fn pve_simulation_is_deterministic() {
        let cfg = MatchConfig::default();
        let d0 = || vec![card("sentinel"), card("vanguard")];
        let d1 = || vec![card("pyromancer")];
        let a = simulate_default(cfg, d0(), d1(), BotDifficulty::Normal, BotDifficulty::Normal);
        let b = simulate_default(cfg, d0(), d1(), BotDifficulty::Normal, BotDifficulty::Normal);
        assert_eq!(a.outcome, b.outcome);
        assert_eq!(a.world.tick, b.world.tick);
    }
}
