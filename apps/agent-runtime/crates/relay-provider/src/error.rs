//! Errors from constructing a Relay-backed model. Runtime completion failures
//! surface as rig's own `rig_core::completion::CompletionError`.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum RelayError {
    /// Failed to build the underlying openai/reqwest client (bad base URL, etc.).
    #[error("failed to build Relay client: {0}")]
    Build(String),
}
