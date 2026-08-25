//! Ledgerline's first product slice: authenticated, idempotent billable-event
//! ingest and request-level revenue/COGS reconciliation.

pub mod domain;
pub mod error;
pub mod mint;
pub mod outbox;
pub mod server;
pub mod settle;
pub mod store;
