//! # aeonrift-moba — the AEONRIFT MOBA game crate
//!
//! One crate, rich internal modules, built ON the shared engine
//! (`aeonrift-sim-core` as `sim`, `aeonrift-ability-dsl` as `ability_dsl`) and the
//! authored `content/` (champions, maps, minions, turrets, objectives, items).
//!
//! ## Module map
//! - [`server`] / [`client`] — the authoritative server + predicting/reconciling
//!   client (the M0/M1 netcode spine; the `no_desync` gate proves zero drift).
//! - [`rules`]   — the MOBA rules: map + content loading, minion waves, lane AI,
//!   turrets/inhibitors/nexus + the win condition, jungle + neutral objectives,
//!   fog-of-war vision, and the item shop. Every entity is a `sim::Champion`, so all
//!   of it fights through the ONE deterministic combat core.
//! - [`game`]    — the match lifecycle: phases, per-team economy, and the [`Match`]
//!   driver that runs the whole MOBA tick on `sim::step`.
//! - [`modes`]   — match-mode configs (normal / ranked / coop-vs-bots).
//! - [`ai`]      — a deterministic laning bot (coop-vs-bots + tests).
//! - [`champions`] — loads the champion roster via `ability_dsl` into playable units.
//!
//! ## Determinism
//! Fixed tick, seeded RNG (only the sim's stream), and id-ordered iteration
//! everywhere. `Match::new(mode, seed)` + the same input sequence yields a
//! byte-identical trajectory (see [`game::Match::fingerprint`]); the netcode spine's
//! `no_desync` determinism is preserved unchanged.

pub mod ai;
pub mod champions;
pub mod client;
pub mod game;
pub mod modes;
pub mod rules;
pub mod server;

pub use client::Client;
pub use game::{Match, Phase};
pub use server::Server;
