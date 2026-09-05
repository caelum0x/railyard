//! Port of the error identity used across the Go service.
//!
//! Go passes the dynamic `error` interface around inside `utils.Result`. The
//! only things the codebase does with it are: read its message string, and
//! compare it against `gorm.ErrRecordNotFound` / `badger.ErrKeyNotFound` to
//! decide whether a failure is non-capturable and non-retryable. `RagoError`
//! preserves exactly that: an opaque message plus a distinguished
//! "record not found" variant. It is `Clone` so `Result` copies cheaply, the
//! way Go copies its value-type `Result` struct.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RagoError {
    /// Mirrors `gorm.ErrRecordNotFound` and `badger.ErrKeyNotFound`.
    RecordNotFound,
    /// Any other error, carrying its `err.Error()` string.
    Message(String),
}

impl RagoError {
    pub fn is_record_not_found(&self) -> bool {
        matches!(self, RagoError::RecordNotFound)
    }

    pub fn message(&self) -> String {
        self.to_string()
    }
}

impl fmt::Display for RagoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // Match gorm's canonical message so any string comparison behaves.
            RagoError::RecordNotFound => write!(f, "record not found"),
            RagoError::Message(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for RagoError {}

impl From<String> for RagoError {
    fn from(s: String) -> Self {
        RagoError::Message(s)
    }
}

impl From<&str> for RagoError {
    fn from(s: &str) -> Self {
        RagoError::Message(s.to_string())
    }
}

impl From<anyhow::Error> for RagoError {
    fn from(e: anyhow::Error) -> Self {
        RagoError::Message(format!("{e}"))
    }
}

impl From<serde_json::Error> for RagoError {
    fn from(e: serde_json::Error) -> Self {
        RagoError::Message(e.to_string())
    }
}

impl From<sqlx::Error> for RagoError {
    fn from(e: sqlx::Error) -> Self {
        match e {
            sqlx::Error::RowNotFound => RagoError::RecordNotFound,
            other => RagoError::Message(other.to_string()),
        }
    }
}

impl From<redis::RedisError> for RagoError {
    fn from(e: redis::RedisError) -> Self {
        RagoError::Message(e.to_string())
    }
}

impl From<rdkafka::error::KafkaError> for RagoError {
    fn from(e: rdkafka::error::KafkaError) -> Self {
        RagoError::Message(e.to_string())
    }
}
