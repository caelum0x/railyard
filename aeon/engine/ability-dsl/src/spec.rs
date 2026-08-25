//! The champion / ability DSL — a small, CLOSED, typed vocabulary.
//!
//! A champion is authored purely as data: base stats plus a list of abilities,
//! where every ability is built from a *fixed, closed* set of [`Effect`]s and one
//! of a *fixed, closed* set of [`Targeting`] modes. There is no escape hatch, no
//! embedded code — the vocabulary is an ontology. Anything outside it fails to
//! parse (serde rejects unknown enum variants) or fails [`crate::validate`].
//!
//! This is the MOAT: adding a champion is authoring data against this vocabulary,
//! not writing (and reviewing) new combat code. The DSL compiles DOWN to the M2
//! sim combat primitives (see [`crate::interpreter`]); it never reimplements them.

use serde::{Deserialize, Serialize};

/// How an ability chooses what it affects. CLOSED set — unknown modes are rejected
/// at parse time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Targeting {
    /// Affects the caster only (e.g. a self-heal / self-buff).
    SelfCast,
    /// Affects a single chosen enemy directly (point-and-click), gated by `range`.
    Target,
    /// Fires a projectile toward an aim point; it damages the first enemy it hits.
    Skillshot,
    /// Instantly affects every enemy within `range` of the aim point.
    Area,
}

/// Optional damage scaling. CLOSED set. Keeps damage a pure function of stats so it
/// stays deterministic and integer-exact after truncation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Scaling {
    /// Adds `ratio * caster.attack_damage` (truncated to an integer) to the base
    /// damage amount.
    AttackDamage { ratio: f32 },
}

/// A single combat verb. This is the CLOSED effect vocabulary — the whole point of
/// the DSL. Every variant maps 1:1 to a public sim combat primitive.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Effect {
    /// Deal `amount` (+ optional `scaling`) pre-mitigation damage.
    Damage {
        amount: i32,
        #[serde(default)]
        scaling: Option<Scaling>,
    },
    /// Restore `amount` HP (never above max).
    Heal { amount: i32 },
    /// Slow the affected champion(s) for `duration` ticks.
    Slow { duration: u32 },
    /// Stun the affected champion(s) for `duration` ticks.
    Stun { duration: u32 },
    /// Dash the caster up to `distance` units toward the aim/target.
    Dash { distance: f32 },
}

/// One ability = a name, a cooldown, a targeting mode, and an ordered list of
/// effects drawn from the closed vocabulary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AbilitySpec {
    pub name: String,
    /// Cooldown in ticks between casts.
    pub cooldown: u32,
    /// Optional resource cost (validated if present; the current sim has no mana
    /// pool, so it is authored + validated but not yet consumed — see README).
    #[serde(default)]
    pub cost: Option<i32>,
    pub targeting: Targeting,
    /// Cast/engage range in world units (unused for `SelfCast`).
    #[serde(default)]
    pub range: f32,
    /// Ordered effects applied on cast. Must be non-empty.
    pub effects: Vec<Effect>,
}

/// Base combat stats for a champion, in DSL-authoring units.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BaseStats {
    /// Maximum health.
    pub hp: i32,
    /// Auto-attack damage (pre-mitigation).
    pub ad: i32,
    /// Flat armor.
    pub armor: i32,
    /// Auto-attacks per second (compiled to an integer tick period).
    pub attack_speed: f32,
    /// Movement speed, world units per second.
    pub move_speed: f32,
    /// Auto-attack range, world units.
    pub range: f32,
}

/// A complete champion, authored purely as data.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChampionSpec {
    pub name: String,
    pub base: BaseStats,
    #[serde(default)]
    pub abilities: Vec<AbilitySpec>,
}
