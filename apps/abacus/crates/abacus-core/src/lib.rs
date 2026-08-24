//! # abacus-core — the Abacus domain kernel.
//!
//! Pure domain types + injectable transport TRAITS with NO HTTP dependency. This
//! crate is the seam every other Abacus crate depends on:
//!
//! - [`ids`] — `AgentId`/`RunId`/`BudgetId` ULID newtypes + the Mint `AccountId`
//!   / provider `Ait1Address` parsed at the boundary.
//! - [`money`] — `Micros` = `i64` µ$ (ADR-0001, no floats).
//! - [`error`] — [`AbacusError`] + `status_for()` mirroring mint error.rs.
//! - [`estimate`] — [`CostEstimate`] + `est_max` (priced MAX possible cost).
//! - [`decision`] — [`EnforcementDecision::Allow`]/`Kill`.
//! - [`transport`] — async [`MintTransport`] + [`UpstreamTransport`] traits + DTOs.
//! - [`identity`] — [`AgentIdentity`] / [`BudgetPolicy`] / async [`BudgetRepo`].
//! - [`receipt`] — [`MintReceipt`] view + async [`ReceiptStore`] system-of-record.
//! - [`envelope`] — the success/error API envelope.
//!
//! The real transport clients live in `abacus-mint-client` / `abacus-upstream`;
//! the in-memory fakes in `abacus-testkit`. Port of `vend-core`.

pub mod decision;
pub mod envelope;
pub mod error;
pub mod estimate;
pub mod identity;
pub mod ids;
pub mod money;
pub mod receipt;
pub mod transport;

pub use decision::EnforcementDecision;
pub use error::AbacusError;
pub use estimate::CostEstimate;
pub use identity::{AgentIdentity, BudgetPolicy, BudgetRepo, FundingPath};
pub use ids::{AccountId, AgentId, Ait1Address, BudgetId, RunId};
pub use money::{Micros, MICROS_PER_USD};
pub use receipt::{MintReceipt, ReceiptOutcome, ReceiptStore};
pub use transport::{
    parse_usage, Allowance, AllowanceSpend, AllowanceSpendReceipt, Capture, CaptureReceipt, Hold,
    HoldReceipt, MintTransport, Release, ReleaseReceipt, UpstreamRequest, UpstreamResponse,
    UpstreamTransport, Usage,
};
