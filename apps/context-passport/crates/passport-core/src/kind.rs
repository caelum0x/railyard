//! What kind of identity a passport locks.

use serde::{Deserialize, Serialize};

use crate::error::PassportError;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PassportKind {
    Character,
    Brand,
    Persona,
}

impl PassportKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Character => "character",
            Self::Brand => "brand",
            Self::Persona => "persona",
        }
    }

    pub fn parse(value: &str) -> Result<Self, PassportError> {
        match value {
            "character" => Ok(Self::Character),
            "brand" => Ok(Self::Brand),
            "persona" => Ok(Self::Persona),
            other => Err(PassportError::Storage(format!(
                "unknown passport kind {other}"
            ))),
        }
    }
}
