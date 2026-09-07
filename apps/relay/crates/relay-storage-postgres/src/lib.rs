//! Independently buildable Postgres persistence for Relay.
//!
//! Keeping these adapters outside the TensorZero gateway binary lets Relay's
//! durability layer compile, lint, and run database integration tests without
//! pulling in the gateway's V8/TypeScript toolchain.

mod challenge;
mod cost_event;
mod evidence;
mod migration;
mod provider_health;
mod settlement_outbox;

pub use challenge::PostgresChallengeStore;
pub use cost_event::PostgresCostEventStore;
pub use evidence::PostgresSettlementEvidenceStore;
pub use migration::{RELAY_MIGRATIONS, RelayMigration, migrate};
pub use provider_health::PostgresProviderHealthStore;
pub use settlement_outbox::{
    BatchClaim, DurableBillingCoordinator, ReservePayload, TerminalPayload,
};
