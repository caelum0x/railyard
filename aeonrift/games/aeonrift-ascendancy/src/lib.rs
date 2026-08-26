//! # Aeonrift: Ascendancy — deterministic lane card-battler
//!
//! The mobile card-strategy game of the **AEONRIFT** franchise (see
//! `AEONRIFT-UNIVERSE.md`). A Clash-Royale-style, headless, engine-neutral rules
//! crate — the same shape and the same determinism law as the MOBA's `sim`:
//! fixed tick, seeded RNG, ordered iteration, and a full-state hash.
//!
//! ## The reuse seam (do NOT reimplement champions)
//! A **card is a champion**. Each card owns a [`champions::ChampionSpec`] — the exact
//! data the MOBA authors and duels with — and adds only a tiny [`cards::CardMeta`]
//! (elixir cost). When deployed, the spec is compiled into a `sim::Champion` via
//! [`champions::compile`], so the unit's hp/ad/armor/range/attack-speed come straight
//! from the shared roster. Deployed units live inside a `sim::World`, so every fight
//! is resolved by the SAME `sim` combat core the MOBA uses.
//!
//! ## Module tree
//! - [`rules`]  — the deterministic core: lanes, elixir, deck/hand cycling, deploy,
//!   towers, win condition, the one-tick runner + state hash.
//! - [`cards`]  — the card layer over the roster: card defs, rarities, the card pool /
//!   collection, and deterministic pack opening (loads `content/cards`).
//! - [`deck`]   — deck building: the deck-building ruleset + the 12 preset decks
//!   (loads `content/cards/decks`).
//! - [`modes`]  — mode configs (ladder / draft / pve) + the PvE match driver.
//! - [`ai`]     — the deterministic opponent bot (shared by pve, the runner, tests).
//! - [`server`] — headless matchmaking / lobby seams.
//! - [`client`] — the Bevy touch client (behind the `client` feature; a thin VIEW).
//!
//! ## Determinism
//! A match is a pure function of `(MatchConfig, decks, deploy decisions)`. Same seed +
//! same deploys => identical final [`match_hash`], always.

pub mod ai;
pub mod cards;
pub mod deck;
pub mod modes;
pub mod rules;
pub mod server;

/// The Bevy touch client — a thin VIEW over the deterministic rules core.
/// Gated behind the `client` feature (pulls Bevy); build with `--features client`.
#[cfg(feature = "client")]
pub mod client;

// ---------------------------------------------------------------------------
// Content directory paths, resolved at compile time relative to this crate. The
// roster (`content/champions`) and card content (`content/cards`) live at the repo
// root; these are the default load paths used by the pool / deck / pack loaders.
// ---------------------------------------------------------------------------
/// Default champion roster directory (`content/champions`).
pub const CONTENT_CHAMPIONS_DIR: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../content/champions");
/// Default card content directory (`content/cards`).
pub const CONTENT_CARDS_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../content/cards");

// ---------------------------------------------------------------------------
// Backward-compatible public surface: the deterministic core the tests, the bots,
// and the migrated client call by these names. Keeping these re-exports stable is
// part of the crate's contract.
// ---------------------------------------------------------------------------
pub use ai::deploy_bot::bot_deploys;
pub use cards::card::{elixir_cost, Card, CardMeta};
pub use cards::face::{default_face_path, face_exists, face_filename, CARD_FACES_DIR};
pub use rules::config::MatchConfig;
pub use rules::deploy::{try_deploy, Deploy, DeployError};
pub use rules::elixir::regen_elixir;
pub use rules::match_state::{match_hash, MatchState, Outcome, BASE0_ID, BASE1_ID, FIRST_UNIT_ID};
pub use rules::runner::{run_bot_match, run_scripted_match, step_tick, DEFAULT_DEPLOY_INTERVAL};
pub use rules::unit_ai::movement_inputs;
