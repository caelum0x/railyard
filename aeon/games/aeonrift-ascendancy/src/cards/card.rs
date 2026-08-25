//! A **card** = a champion, reused as a deployable unit.
//!
//! THE REUSE SEAM. A [`Card`] owns a [`champions::ChampionSpec`] verbatim — the same
//! data the MOBA authors and duels with — plus a tiny [`CardMeta`] (its elixir cost).
//! The champion's hp/ad/armor/range/attack-speed become the unit's combat body by
//! compiling the spec into a `sim::Champion` via [`champions::compile`]. Nothing about
//! a champion's stats is re-authored here; the card layer only adds economy metadata.

use champions::ChampionSpec;
use sim::{Champion, EntityId, Vec2};

/// Card-game metadata layered on top of a champion spec. Currently just the elixir
/// cost, derived deterministically from the champion's raw power (see [`elixir_cost`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CardMeta {
    /// Elixir required to deploy this card.
    pub cost: u32,
}

/// A deployable card: a champion spec + its derived economy metadata.
#[derive(Clone, Debug, PartialEq)]
pub struct Card {
    pub spec: ChampionSpec,
    pub meta: CardMeta,
}

/// Minimum and maximum elixir a card can cost (Clash-style 1..=10 band).
pub const MIN_COST: u32 = 1;
pub const MAX_COST: u32 = 10;

/// Divisor turning a champion's raw power score into an elixir cost. Chosen so the
/// example roster lands in a sensible 2..=4 band. Mirrored in content/cards/tuning.ron
/// (see [`crate::cards::rarity`] / tooling), but kept as a constant here so pricing a
/// card needs no file IO on the default path.
pub const POWER_DIVISOR: i64 = 300;

/// Derive a card's elixir cost from its champion's base stats, deterministically and
/// with pure integer math (no floats => bit-identical on every machine).
///
/// `power = hp + ad*8 + armor*6`, then `cost = clamp(power / DIVISOR, 1, 10)`. HP,
/// damage, and durability all push the cost up, so a beefier champion costs more.
pub fn elixir_cost(spec: &ChampionSpec) -> u32 {
    let b = &spec.base;
    let power: i64 = b.hp as i64 + (b.ad as i64) * 8 + (b.armor as i64) * 6;
    let raw = (power / POWER_DIVISOR).clamp(MIN_COST as i64, MAX_COST as i64);
    raw as u32
}

impl Card {
    /// Build a card from a champion spec, deriving its elixir cost.
    pub fn from_spec(spec: ChampionSpec) -> Self {
        let cost = elixir_cost(&spec);
        Card {
            spec,
            meta: CardMeta { cost },
        }
    }

    /// The card's display name (its champion's name).
    pub fn name(&self) -> &str {
        &self.spec.name
    }

    /// Compile this card into a live `sim::Champion` — the unit that walks the lane
    /// and fights. This is the single call that turns champion DATA into a combat
    /// body; all stats come from the shared spec via [`champions::compile`].
    pub fn spawn_unit(&self, id: EntityId, team: u8, pos: Vec2) -> Champion {
        champions::compile(&self.spec, id, team, pos)
    }
}
