//! # `rules` — the deterministic lane card-battler core.
//!
//! This is the headless, engine-neutral rules layer of Aeonrift: Ascendancy. It obeys
//! the franchise determinism law — fixed tick, seeded RNG, ordered iteration, full
//! state hash — and reuses the shared `sim` combat core for every fight (units and
//! bases are `sim::Champion`s living in a `sim::World`).
//!
//! Module map:
//! - [`config`]      — [`MatchConfig`] + tunable defaults.
//! - [`lanes`]       — lane / base geometry (pure functions of the config).
//! - [`towers`]      — bases as stationary combat bodies.
//! - [`elixir`]      — the deterministic elixir economy.
//! - [`deck`] + [`hand`] — Clash-style seeded card cycling (layered on top).
//! - [`deploy`]      — the one player action, validated at the boundary.
//! - [`unit_ai`]     — deterministic march-or-hold movement intents.
//! - [`match_state`] — the full hashed match state.
//! - [`win`]         — the base-destroyed win condition.
//! - [`runner`]      — the one-tick pipeline + scripted / bot match drivers.

pub mod config;
pub mod deck;
pub mod deploy;
pub mod elixir;
pub mod hand;
pub mod lanes;
pub mod match_state;
pub mod runner;
pub mod towers;
pub mod unit_ai;
pub mod win;

pub use config::MatchConfig;
pub use deck::DrawPile;
pub use deploy::{march_input, try_deploy, Deploy, DeployError};
pub use elixir::{can_afford, regen_elixir};
pub use hand::{Hand, HAND_SIZE};
pub use match_state::{match_hash, MatchState, Outcome, BASE0_ID, BASE1_ID, FIRST_UNIT_ID};
pub use runner::{run_bot_match, run_scripted_match, step_tick, DEFAULT_DEPLOY_INTERVAL};
pub use towers::build_base;
pub use unit_ai::movement_inputs;
