//! The deterministic opponent bot.
//!
//! A pure policy over [`MatchState`]: given the current state it returns this tick's
//! deploy decisions. It is used by the bot-vs-bot match runner, by PvE, and by the
//! tests as a reproducible opponent. No RNG, no wall-clock — the same state always
//! yields the same move, which is what keeps bot matches deterministic.

use crate::rules::deploy::Deploy;
use crate::rules::match_state::MatchState;

/// How the bot chooses which affordable card to play.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BotDifficulty {
    /// Dump the cheapest affordable card as fast as possible (weakest tempo).
    Easy,
    /// Play the most expensive card it can currently afford (the balanced default).
    #[default]
    Normal,
    /// Like Normal, but only commits once it can afford a mid-cost threat, banking
    /// elixir for stronger units (steadier value).
    Hard,
}

/// The bot's deploy decisions for the current tick, for BOTH players, using the
/// default (`Normal`) difficulty. This is the policy the bot-vs-bot runner uses;
/// keeping it stable preserves the match regression baselines.
pub fn bot_deploys(m: &MatchState, interval: u64) -> Vec<Deploy> {
    let mut out = Vec::new();
    let t = m.world.tick;
    if t == 0 || interval == 0 || !t.is_multiple_of(interval) {
        return out;
    }
    for player in 0u8..2 {
        if let Some(d) = pick_deploy(m, player, BotDifficulty::Normal) {
            out.push(d);
        }
    }
    out
}

/// One player's deploy decision for this tick at the given difficulty, ignoring the
/// deploy-interval gate (useful for PvE where the human's cadence differs).
pub fn bot_deploy_for(
    m: &MatchState,
    player: u8,
    difficulty: BotDifficulty,
) -> Option<Deploy> {
    pick_deploy(m, player, difficulty)
}

/// Pick a player's deploy at `difficulty`. Ties break by lowest deck index; the unit
/// is placed into the next lane in rotation (by that player's deployed count). Returns
/// `None` if nothing suitable is affordable.
fn pick_deploy(m: &MatchState, player: u8, difficulty: BotDifficulty) -> Option<Deploy> {
    let p = player as usize;
    let deck = m.decks.get(p)?;
    let elixir = m.elixir[p];

    // `Hard` banks elixir until it can afford at least a mid-cost card.
    let min_cost = match difficulty {
        BotDifficulty::Hard => deck.iter().map(|c| c.meta.cost).max().unwrap_or(0).min(4),
        _ => 0,
    };

    let mut best: Option<(usize, u32)> = None;
    for (i, card) in deck.iter().enumerate() {
        let cost = card.meta.cost;
        if cost > elixir || cost < min_cost {
            continue;
        }
        let better = match best {
            None => true,
            Some((_, bc)) => match difficulty {
                // Easy prefers the cheapest; the others prefer the most expensive.
                BotDifficulty::Easy => cost < bc,
                BotDifficulty::Normal | BotDifficulty::Hard => cost > bc,
            },
        };
        if better {
            best = Some((i, cost));
        }
    }

    let (idx, _) = best?;
    let lane = (m.deployed_count[p] as usize) % m.config.lanes.max(1);
    Some(Deploy::new(player, idx, lane))
}
