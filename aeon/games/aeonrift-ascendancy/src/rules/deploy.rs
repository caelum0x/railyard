//! Deploying a card into a lane — the one action a player takes.
//!
//! A deploy is validated at the boundary (input validation): the card index and lane
//! must exist and the player must be able to afford the card's elixir cost. Only then
//! is elixir spent and a unit spawned into the shared sim world. Rejections are
//! explicit [`DeployError`]s, never silent — so "can't deploy without enough elixir"
//! is an observable, testable rule.

use sim::{EntityId, Input};

use super::elixir::can_afford;
use super::match_state::MatchState;

/// A request to deploy `card` into `lane` by `player`. Pure data, so a whole match's
/// deploys can be scripted and replayed deterministically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deploy {
    pub player: u8,
    /// Index into that player's deck.
    pub card: usize,
    /// Target lane (0-based).
    pub lane: usize,
}

impl Deploy {
    pub fn new(player: u8, card: usize, lane: usize) -> Self {
        Deploy { player, card, lane }
    }
}

/// Why a deploy was rejected. Every rejection is one of these — never a silent no-op.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeployError {
    /// `player` was not 0 or 1.
    BadPlayer,
    /// The card index does not exist in the player's deck.
    BadCard,
    /// The lane index is out of range for the match's lane count.
    BadLane,
    /// The player has fewer elixir than the card's cost.
    NotEnoughElixir { have: u32, need: u32 },
    /// The match has already ended.
    MatchOver,
}

/// Attempt a deploy against the current state. On success, spends elixir, spawns the
/// unit into the sim world (ascending id order preserved), and returns the new unit's
/// entity id. On failure returns a precise [`DeployError`] and leaves state untouched.
pub fn try_deploy(m: &mut MatchState, d: Deploy) -> Result<EntityId, DeployError> {
    if m.is_over() {
        return Err(DeployError::MatchOver);
    }
    if d.player > 1 {
        return Err(DeployError::BadPlayer);
    }
    let p = d.player as usize;

    let card = m
        .decks
        .get(p)
        .and_then(|deck| deck.get(d.card))
        .ok_or(DeployError::BadCard)?
        .clone();

    if d.lane >= m.config.lanes {
        return Err(DeployError::BadLane);
    }

    let cost = card.meta.cost;
    let have = m.elixir[p];
    if !can_afford(have, cost) {
        return Err(DeployError::NotEnoughElixir { have, need: cost });
    }

    // --- All checks passed: commit the deploy. ---
    let id = m.next_unit_id;
    m.next_unit_id += 1;

    let pos = m.config.spawn_pos(d.player, d.lane);
    let mut unit = card.spawn_unit(id, d.player, pos);
    // The unit immediately marches toward the enemy base; the per-tick AI refines
    // this (holding to fight enemies in range). Setting it here means a unit deployed
    // this tick already has intent for the very next movement step.
    unit.target = Some(m.config.base_pos(1 - d.player));

    m.world.champions.push(unit); // ids handed out monotonically => stays sorted
    m.elixir[p] -= cost;
    m.deployed_count[p] += 1;

    Ok(id)
}

/// Build the "march toward the enemy base" input for a unit, used by the per-tick AI
/// when the unit has no enemy in auto-attack range.
pub fn march_input(m: &MatchState, unit_id: EntityId, owner: u8) -> Input {
    Input::move_to(unit_id, m.config.base_pos(1 - owner))
}
