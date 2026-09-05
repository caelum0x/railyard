//! # rago-core
//!
//! Core domain layer for **Rago**, the Rust port of Lago's Ruby billing engine
//! (`lago-api`). Contains the value types every other crate builds on:
//!
//! * [`money`] — [`money::Currency`] and Ruby-`BigDecimal`-compatible rounding.
//! * [`enums`] — the ActiveRecord integer enums, faithfully mapped.
//! * [`models`] — the billing entities (Organization, Customer, BillableMetric,
//!   Plan, Charge, Subscription, Fee, Invoice, Wallet, Coupon, Credit,
//!   CreditNote, Tax, Event, …) as plain structs mirroring `db/structure.sql`.
//!
//! Money is always exact: integer cents (`i64`) plus [`rust_decimal::Decimal`]
//! for precise fractional-cent columns. Floating point is never used for money.

pub mod enums;
pub mod models;
pub mod money;

pub use money::Currency;
