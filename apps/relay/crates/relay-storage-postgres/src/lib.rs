//! Independently buildable Postgres persistence for Relay.
//!
//! Keeping these adapters outside the TensorZero gateway binary lets Relay's
//! durability layer compile, lint, and run database integration tests without
//! pulling in the gateway's V8/TypeScript toolchain.

mod billing_summary;
mod budget;
mod catalog_price;
mod challenge;
mod cost_event;
mod evidence;
mod metered_batch;
mod migration;
mod provider_credential;
mod provider_health;
mod settlement_outbox;

pub use billing_summary::{
    DEFAULT_SETTLEMENT_HISTORY_LIMIT, MAX_SETTLEMENT_HISTORY_LIMIT, OPERATOR_SCOPE_PRINCIPAL,
    PostgresBillingSummaryStore,
};
pub use budget::{
    BudgetDecision, BudgetPeriod, KeyBudgetError, KeyBudgetInput, KeyBudgetStatus,
    MAX_BUDGET_NOTE_CHARS, PostgresKeyBudgetStore,
};
pub use catalog_price::{
    CatalogPriceError, CatalogPriceInput, CatalogPriceOverride, MAX_PRICE_MICRO_USD_PER_MILLION,
    MAX_PRICE_NOTE_CHARS, PostgresCatalogPriceStore,
};
pub use challenge::PostgresChallengeStore;
pub use cost_event::PostgresCostEventStore;
pub use evidence::PostgresSettlementEvidenceStore;
pub use metered_batch::{MeteredBatch, PostgresMeteredBatchStore};
pub use migration::{RELAY_MIGRATIONS, RelayMigration, migrate};
pub use provider_credential::{
    PostgresProviderCredentialStore, ProviderCredentialTest, SealedProviderCredential,
};
pub use provider_health::PostgresProviderHealthStore;
pub use settlement_outbox::{
    BatchClaim, DurableBillingCoordinator, ReservePayload, TerminalPayload,
};
