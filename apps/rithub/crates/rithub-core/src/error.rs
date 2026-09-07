//! Typed domain errors. The store maps storage-level failures (unique
//! violations, missing rows) onto these so callers never see raw SQL text.

use thiserror::Error;

/// The result type used across the forge domain.
pub type Result<T> = std::result::Result<T, CoreError>;

/// A forge-domain error.
#[derive(Debug, Error)]
pub enum CoreError {
    /// A name (org/repo/user) failed validation at the boundary.
    #[error("invalid {kind} name: {value}")]
    InvalidName { kind: &'static str, value: String },

    /// The requested entity does not exist.
    #[error("{kind} not found: {key}")]
    NotFound { kind: &'static str, key: String },

    /// An entity with a unique key already exists.
    #[error("{kind} already exists: {key}")]
    Conflict { kind: &'static str, key: String },

    /// The underlying database failed for a reason not otherwise classified.
    #[error("database error: {0}")]
    Database(String),
}

impl CoreError {
    /// Classify a raw `sqlx::Error` into a domain error, folding unique
    /// violations into [`CoreError::Conflict`] and missing rows into
    /// [`CoreError::NotFound`] where the caller supplies context.
    pub fn from_sqlx(kind: &'static str, key: &str, error: sqlx::Error) -> Self {
        match &error {
            sqlx::Error::RowNotFound => CoreError::NotFound {
                kind,
                key: key.to_string(),
            },
            sqlx::Error::Database(db) if db.is_unique_violation() => CoreError::Conflict {
                kind,
                key: key.to_string(),
            },
            _ => CoreError::Database(error.to_string()),
        }
    }
}
