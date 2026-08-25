//! Audio error type.

use std::fmt;

/// Errors surfaced by the audio engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioError {
    /// The audio device / backend could not be initialized.
    Backend(String),
    /// A sound name was requested that has not been loaded.
    NotLoaded(String),
    /// Decoding encoded audio bytes failed.
    Decode(String),
    /// The mixer rejected a play request (e.g. capacity reached).
    Play(String),
}

impl fmt::Display for AudioError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AudioError::Backend(e) => write!(f, "audio backend error: {e}"),
            AudioError::NotLoaded(n) => write!(f, "sound not loaded: {n}"),
            AudioError::Decode(e) => write!(f, "audio decode error: {e}"),
            AudioError::Play(e) => write!(f, "audio play error: {e}"),
        }
    }
}

impl std::error::Error for AudioError {}
