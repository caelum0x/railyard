//! The kind of output a caller wants to condition.

use passport_core::PassportError;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Modality {
    #[default]
    Image,
    Voice,
    Video,
    Text,
}

impl Modality {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Voice => "voice",
            Self::Video => "video",
            Self::Text => "text",
        }
    }

    pub fn parse(value: &str) -> Result<Self, PassportError> {
        match value {
            "image" => Ok(Self::Image),
            "voice" => Ok(Self::Voice),
            "video" => Ok(Self::Video),
            "text" => Ok(Self::Text),
            other => Err(PassportError::ResolveUnavailable {
                provider: "*".into(),
                modality: other.into(),
            }),
        }
    }
}
