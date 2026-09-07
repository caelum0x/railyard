//! Errors for the git-hosting layer.

use thiserror::Error;

/// Result alias for the git-hosting layer.
pub type Result<T> = std::result::Result<T, GitHostError>;

/// A git-hosting error.
#[derive(Debug, Error)]
pub enum GitHostError {
    /// A path component or request was malformed / unsafe.
    #[error("invalid request: {0}")]
    Invalid(String),

    /// A requested resource (object, pack, HEAD) does not exist.
    #[error("not found: {0}")]
    NotFound(String),

    /// A `rit` operation failed.
    #[error("git error: {0}")]
    Rit(#[from] rit::ForgeError),

    /// A filesystem error.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
