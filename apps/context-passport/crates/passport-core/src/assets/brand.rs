//! Locked, typed asset fields for a BRAND identity.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrandAssets {
    #[serde(default)]
    pub logo_ref: Option<String>,
    /// Palette as hex color strings.
    #[serde(default)]
    pub palette: Vec<String>,
    #[serde(default)]
    pub typography: Option<String>,
    #[serde(default)]
    pub watermark_ref: Option<String>,
    /// Voice-and-tone guidelines.
    #[serde(default)]
    pub voice_tone: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrandPatch {
    #[serde(default)]
    pub logo_ref: Option<String>,
    #[serde(default)]
    pub palette: Option<Vec<String>>,
    #[serde(default)]
    pub typography: Option<String>,
    #[serde(default)]
    pub watermark_ref: Option<String>,
    #[serde(default)]
    pub voice_tone: Option<String>,
}

impl BrandAssets {
    pub fn apply(&self, patch: &BrandPatch) -> Self {
        let mut next = self.clone();
        if let Some(v) = &patch.logo_ref {
            next.logo_ref = Some(v.clone());
        }
        if let Some(v) = &patch.palette {
            next.palette = v.clone();
        }
        if let Some(v) = &patch.typography {
            next.typography = Some(v.clone());
        }
        if let Some(v) = &patch.watermark_ref {
            next.watermark_ref = Some(v.clone());
        }
        if let Some(v) = &patch.voice_tone {
            next.voice_tone = Some(v.clone());
        }
        next
    }
}
