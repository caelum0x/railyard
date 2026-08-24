//! `mint-store-postgres` — the production Postgres [`mint_store::LedgerStore`].
//!
//! Implements the frozen `LedgerStore` trait against Postgres using the sqlx
//! RUNTIME query API (`sqlx::query` / `.bind`), NEVER the compile-time `query!`
//! macros, so the crate builds with NO live `DATABASE_URL`. The atomicity the
//! `InMemoryStore` expresses with one `Mutex` is expressed here with one SQL
//! transaction + `SELECT ... FOR UPDATE` row locks taken in `AccountId` sort
//! order (deadlock-free). The append-only journal + hash-linked receipt chain
//! are byte-compatible with the in-memory fold, so the AP0 rebuild gate holds.
//!
//! - [`store`]   — [`PgStore`] + the `LedgerStore` impl.
//! - [`tx`]      — the `FOR UPDATE` / journal / receipt / idempotency helpers.
//! - [`rows`]    — BIGINT µ$ ↔ `Micros`, smallint ↔ enums, bytea ↔ hashes.
//! - [`rebuild`] — the journal-fold rebuild of every projection.
//! - [`migrate`] — [`run_migrations`] (embedded, forward-only).

pub mod migrate;
pub mod rebuild;
pub mod rows;
pub mod store;
pub mod tx;

pub use migrate::run_migrations;
pub use store::{PgStore, PgStoreConfig};
