//! Ledgerline's first product slice: authenticated, idempotent billable-event
//! ingest and request-level revenue/COGS reconciliation.

pub mod domain;
pub mod error;
pub mod mint;
pub mod server;
pub mod store;
