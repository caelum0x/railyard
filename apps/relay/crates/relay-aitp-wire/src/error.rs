use thiserror::Error;

/// Failures decoding Relay's AITP header vocabulary.
#[derive(Debug, Error)]
pub enum WireError {
    #[error("AITP header codec error: {0}")]
    Codec(String),
}
