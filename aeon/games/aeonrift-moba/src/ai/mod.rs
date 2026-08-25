//! Deterministic bot AI.
//!
//! Currently a single [`laning_bot`] used for the coop-vs-bots RED team and by the
//! scripted match tests. It emits only [`sim::Input`]s, so bots are indistinguishable
//! from players at the sim boundary and stay fully deterministic.

pub mod laning_bot;

pub use laning_bot::decide;
