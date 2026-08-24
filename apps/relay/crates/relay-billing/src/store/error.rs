//! Storage-layer error type, convertible into the core `RelayError` taxonomy.

use thiserror::Error;

use crate::error::RelayError;

/// A failure from the storage seam. Kept deliberately small — the in-memory
/// impl can only fail on a poisoned lock or a micro-sum overflow; a future
/// `PgStore` adds its own variants behind the same `Store` trait.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StoreError {
    /// The ledger sum overflowed i64 — a reconciliation bug, never silent.
    #[error("ledger balance overflow for account {account}")]
    Overflow { account: String },

    /// An internal lock was poisoned by a panicking holder.
    #[error("store lock poisoned")]
    Lock,

    /// Any other backend failure (reserved for the future `PgStore`).
    #[error("store backend error: {0}")]
    Backend(String),
}

impl From<StoreError> for RelayError {
    fn from(e: StoreError) -> Self {
        RelayError::Store(e.to_string())
    }
}
