//! Ledgerline's first product slice: authenticated, idempotent billable-event
//! ingest and request-level revenue/COGS reconciliation.

pub mod domain;
pub mod error;
pub mod mint;
pub mod outbox;
pub mod recovery;
pub mod server;
pub mod settle;
pub mod store;

/// Crate-internal, shared test doubles (in-memory Mint, fixtures, assertions).
/// Compiled only under test; used by the settle and recovery test suites.
#[cfg(test)]
pub(crate) mod test_support;
