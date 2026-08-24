//! # abacus-testkit — FAKE transports + fixtures for TDD (no live server, no DB).
//!
//! - [`LedgerFakeMint`] — an in-memory Mint that ENFORCES the balance/allowance
//!   ceiling invariant with a serialized per-account apply, so N concurrent holds
//!   cannot collectively overshoot; records every call and returns a real `402`
//!   when `est_max` would breach the ceiling.
//! - [`ScriptedUpstream`] — canned success-with-usage / failure / timeout.
//! - [`fixtures`] — seed agents/budgets plus in-memory [`FakeBudgetRepo`] +
//!   [`FakeReceiptStore`].
//!
//! Mirrors `vend-testkit` + `aitp-client` `stub.rs`.

pub mod fake_mint;
pub mod fake_upstream;
pub mod fixtures;

pub use fake_mint::{Call, LedgerFakeMint};
pub use fake_upstream::ScriptedUpstream;
pub use fixtures::{FakeBudgetRepo, FakeReceiptStore};
