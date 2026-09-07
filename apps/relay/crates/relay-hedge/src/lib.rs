//! relay-hedge: additive hedged dispatch + per-endpoint SLO/health routing for
//! the TensorZero (`apps/relay`) gateway.
//!
//! This crate is a *layer around* endpoint selection, not a replacement for
//! TensorZero's provider dispatch. It provides two independent, opt-in pieces:
//!
//! * [`hedge`] — a hedged race over N deferred provider attempts. It launches up
//!   to `max_concurrency` attempts (staggered, cost-capped), returns the FIRST
//!   `Ok` (first-token wins), and DROPS every losing attempt. Dropping the
//!   in-flight future cancels the underlying upstream call at the socket, so a
//!   loser never produces a response.
//! * [`slo`] / [`score`] / [`health_store`] — an EWMA-based per-endpoint health
//!   scorer with a circuit breaker. It only *reorders* the routing slice before
//!   dispatch; it never changes how a provider is actually called.
//!
//! ## Billing safety (the load-bearing invariant)
//!
//! [`hedge::race`] returns EXACTLY ONE winning value and drops all losers. The
//! caller therefore propagates exactly one response (with exactly one `usage`
//! block) up to the success-only billing middleware, which settles that one
//! response. Losing attempts are dropped inside the race: they never yield a
//! value, never propagate `usage`, and never reach billing => never charged.
//! The [`cost`] cap only bounds concurrent *upstream* (vendor-side) spend; it is
//! never a ledger charge. See `tests/billing_safety.rs` for the executable proof
//! against the real `relay_billing::Ledger`.
//!
//! Defaults preserve current behavior: [`config::HedgeConfig::default`] is
//! `enabled = false`, `max_concurrency = 1`, which takes the sequential code
//! path — byte-for-byte the existing one-provider-at-a-time loop.

pub mod config;
pub mod cost;
pub mod health_store;
pub mod hedge;
pub mod score;
pub mod slo;

pub use config::HedgeConfig;
pub use cost::{worst_case_cost_micros, worst_case_cost_micros_per_million};
pub use health_store::InMemoryProviderHealth;
pub use health_store::ProviderHealthSnapshot;
pub use hedge::{race, Attempt, HedgeError, HedgeOutcome};
pub use score::{CircuitState, EwmaScore, ScoreWeights, SloConfig};
pub use slo::{NoOpProviderHealth, Outcome, ProviderHealth, ResultClass};
