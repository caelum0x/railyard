//! `champions` — M5: the champion-codegen ability DSL (the MOAT).
//!
//! A champion is authored as **data** against a small, CLOSED, typed vocabulary,
//! then compiled DOWN to the deterministic M2 sim combat core — it never
//! reimplements combat. Adding a champion to the roster is therefore authoring +
//! validating data, not writing and reviewing new combat code. That is what lets
//! the roster scale by codegen instead of headcount.
//!
//! Pipeline:
//! 1. [`spec`] — the DSL types (`ChampionSpec`, `AbilitySpec`, `Effect`,
//!    `Targeting`). Effect/targeting vocabularies are closed enums.
//! 2. [`loader`] + [`validate`] — parse RON (structural: unknown effects/targeting
//!    are rejected here) then semantically validate. This is the single gate that
//!    kills malformed champions.
//! 3. [`interpreter`] — compile a spec's base stats into a `sim::Champion` and
//!    execute its abilities via the sim's public combat primitives.
//! 4. [`duel`] — a spec-driven, deterministic 1v1 duel harness producing an
//!    assertable result (winner, HP, ticks, hash): the green/red gate for new
//!    champions.

pub mod duel;
pub mod interpreter;
pub mod loader;
pub mod spec;
pub mod validate;

pub use duel::{run_duel, DuelConfig, DuelResult};
pub use interpreter::{compile, execute_ability, ChampionRuntime};
pub use loader::{load_file, load_str};
pub use spec::{AbilitySpec, BaseStats, ChampionSpec, Effect, Scaling, Targeting};
pub use validate::{validate, SpecError};
