//! The battler's full match state and its deterministic state hash.
//!
//! A [`MatchState`] wraps a `sim::World` (which holds every combat body — bases and
//! deployed units — as `sim::Champion`s) and adds the card layer: per-player decks,
//! elixir, base ids, and the match outcome. Because units live *inside* the sim
//! world, the sim's own combat + movement + projectile systems resolve every fight;
//! the card layer only owns economy + deploy + win-condition state.

use sim::{state_hash, EntityId, World};

use super::config::MatchConfig;
use super::towers::build_base;
use crate::cards::card::Card;

/// Ids 0 and 1 are always the two bases (towers); deployed units get ids >= 2.
pub const BASE0_ID: EntityId = 0;
pub const BASE1_ID: EntityId = 1;
/// First id handed out to a deployed unit (bases occupy 0 and 1).
pub const FIRST_UNIT_ID: EntityId = 2;

/// How a match ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// `player` (0 or 1) destroyed the opponent's base.
    Winner(u8),
    /// Both bases fell on the same tick, or the tick cap was reached.
    Draw,
}

/// The complete, deterministic state of one match. `PartialEq` powers full-state
/// equality checks in the determinism gate (every field, not just the hash).
#[derive(Clone, Debug, PartialEq)]
pub struct MatchState {
    pub config: MatchConfig,
    /// The shared deterministic sim world: bases + all deployed units live here.
    pub world: World,
    /// Each player's deck of deployable cards. Index into this is a card id.
    pub decks: [Vec<Card>; 2],
    /// Current elixir per player.
    pub elixir: [u32; 2],
    /// Entity ids of the two bases, indexed by player.
    pub base_ids: [EntityId; 2],
    /// Monotonic id source for the next deployed unit (never reused => stable hash).
    pub next_unit_id: EntityId,
    /// Number of units deployed by each player (a lightweight match statistic).
    pub deployed_count: [u32; 2],
    /// `Some` once the match has resolved; further ticks are no-ops.
    pub outcome: Option<Outcome>,
}

impl MatchState {
    /// Create a fresh match: a seeded world containing exactly the two bases, each
    /// player's deck, and their starting elixir. No units are deployed yet.
    pub fn new(config: MatchConfig, deck0: Vec<Card>, deck1: Vec<Card>) -> Self {
        let base0 = build_base(BASE0_ID, 0, &config);
        let base1 = build_base(BASE1_ID, 1, &config);
        // Bases supplied in ascending id order (sim requires deterministic ordering).
        let world = World::from_champions(config.seed, vec![base0, base1]);

        MatchState {
            config,
            world,
            decks: [deck0, deck1],
            elixir: [config.start_elixir, config.start_elixir],
            base_ids: [BASE0_ID, BASE1_ID],
            next_unit_id: FIRST_UNIT_ID,
            deployed_count: [0, 0],
            outcome: None,
        }
    }

    /// Is this entity id one of the two bases (and therefore immobile)?
    pub fn is_base(&self, id: EntityId) -> bool {
        id == self.base_ids[0] || id == self.base_ids[1]
    }

    /// Current HP of `player`'s base (0 if it has been destroyed / is gone).
    pub fn base_hp(&self, player: u8) -> i32 {
        self.world
            .champion(self.base_ids[player as usize])
            .map(|c| c.health.current)
            .unwrap_or(0)
    }

    /// Is `player`'s base still standing?
    pub fn base_alive(&self, player: u8) -> bool {
        self.world
            .champion(self.base_ids[player as usize])
            .map(|c| c.alive)
            .unwrap_or(false)
    }

    /// Has the match resolved?
    pub fn is_over(&self) -> bool {
        self.outcome.is_some()
    }
}

/// FNV-1a-style mix of a running 64-bit hash with one more 64-bit word.
fn mix_u64(mut h: u64, v: u64) -> u64 {
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    for shift in [0, 8, 16, 24, 32, 40, 48, 56] {
        let byte = (v >> shift) & 0xff;
        h ^= byte;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// The card-game state hash: the full `sim` world hash folded together with the card
/// layer's own state (tick, elixir, unit id source, deploy counts, outcome). Two
/// matches with identical content produce an identical hash; any drift is detected.
pub fn match_hash(m: &MatchState) -> u64 {
    let mut h = state_hash(&m.world);
    h = mix_u64(h, m.world.tick);
    h = mix_u64(h, m.elixir[0] as u64);
    h = mix_u64(h, m.elixir[1] as u64);
    h = mix_u64(h, m.next_unit_id as u64);
    h = mix_u64(h, m.deployed_count[0] as u64);
    h = mix_u64(h, m.deployed_count[1] as u64);
    let outcome_code = match m.outcome {
        None => 0,
        Some(Outcome::Winner(0)) => 1,
        Some(Outcome::Winner(_)) => 2,
        Some(Outcome::Draw) => 3,
    };
    mix_u64(h, outcome_code)
}
