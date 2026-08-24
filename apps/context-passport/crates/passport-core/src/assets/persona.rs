//! Locked, typed asset fields for a PERSONA identity.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaAssets {
    #[serde(default)]
    pub audience: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    /// Target platform conventions (tiktok, reels, shorts, …).
    #[serde(default)]
    pub platform: Option<String>,
    /// Hooks that convert for this persona.
    #[serde(default)]
    pub hooks: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaPatch {
    #[serde(default)]
    pub audience: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub hooks: Option<Vec<String>>,
}

impl PersonaAssets {
    pub fn apply(&self, patch: &PersonaPatch) -> Self {
        let mut next = self.clone();
        if let Some(v) = &patch.audience {
            next.audience = Some(v.clone());
        }
        if let Some(v) = &patch.language {
            next.language = Some(v.clone());
        }
        if let Some(v) = &patch.platform {
            next.platform = Some(v.clone());
        }
        if let Some(v) = &patch.hooks {
            next.hooks = v.clone();
        }
        next
    }
}
